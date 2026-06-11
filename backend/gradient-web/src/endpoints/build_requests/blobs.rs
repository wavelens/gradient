/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/{session}/blobs` - accepts multipart form data
//! where each part is named by its BLAKE3 hex hash. Verifies hashes,
//! persists payloads via `nar_storage.put_blob`, and shrinks the session's
//! `missing` set as blobs arrive.

use super::validation::decode_blake3_hex;
use crate::access::has_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::Extension;
use axum::Json;
use axum::extract::{Multipart, Path, State};
use gradient_types::ids::{BuildRequestBlobId, UploadSessionId};
use gradient_types::{
    AUploadSession, BaseResponse, EUploadSession, MBuildRequestBlob, MUser, now,
};
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, DbErr, EntityTrait, IntoActiveModel, RuntimeErr, sqlx};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct BlobsResponse {
    pub uploaded: usize,
    pub remaining: usize,
}

pub async fn post_blobs(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(session_id): Path<UploadSessionId>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<BlobsResponse>>> {
    let session = EUploadSession::find_by_id(session_id)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Upload session"))?;

    if session.dispatched_at.is_some() {
        return Err(WebError::conflict("Upload session already dispatched"));
    }

    if now() > session.expires_at {
        return Err(WebError::gone("Upload session expired"));
    }

    let api_key_ref = api_key.as_ref();
    if !has_permission(
        &state,
        user.id,
        session.organization,
        Permission::TriggerEvaluation,
        api_key_ref,
    )
    .await?
    {
        return Err(WebError::not_found("Upload session"));
    }

    let missing_vec: Vec<String> = serde_json::from_value(session.missing.clone())
        .map_err(|e| WebError::internal(format!("Corrupt session.missing JSON: {}", e)))?;
    let mut missing_set: HashSet<String> = missing_vec.into_iter().collect();

    let org_uuid = session.organization.into_inner();
    let mut uploaded: usize = 0;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::bad_request(format!("Invalid multipart payload: {}", e)))?
    {
        let name = field
            .name()
            .ok_or_else(|| WebError::bad_request("Missing field name"))?
            .to_string();

        let hash_bytes = decode_blake3_hex(&name)?;
        if !missing_set.contains(&name) {
            return Err(WebError::bad_request(format!(
                "hash not in session.missing: {}",
                name
            )));
        }

        let data = field
            .bytes()
            .await
            .map_err(|e| WebError::bad_request(format!("Failed to read field bytes: {}", e)))?;

        let actual = blake3::hash(&data);
        if actual.as_bytes() != hash_bytes.as_slice() {
            return Err(WebError::bad_request(format!("hash mismatch for {}", name)));
        }

        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hash_bytes);
        let size = data.len() as i64;
        let bytes_vec = data.to_vec();

        state
            .nar_storage
            .put_blob(org_uuid, &hash_array, bytes_vec)
            .await
            .map_err(|e| WebError::internal(format!("Failed to persist blob: {}", e)))?;

        let now_ts = now();
        let insert_result = MBuildRequestBlob {
            id: BuildRequestBlobId::now_v7(),
            organization: session.organization,
            hash: hash_bytes.clone(),
            size,
            created_at: now_ts,
            last_used_at: now_ts,
        }
        .into_active_model()
        .insert(&state.web_db)
        .await;

        match insert_result {
            Ok(_) => {}
            Err(err) if is_unique_violation(&err) => {}
            Err(err) => return Err(err.into()),
        }

        missing_set.remove(&name);
        uploaded += 1;
    }

    let remaining_vec: Vec<String> = missing_set.into_iter().collect();
    let remaining = remaining_vec.len();
    let missing_value = serde_json::to_value(&remaining_vec)
        .map_err(|e| WebError::internal(format!("Failed to serialise missing list: {}", e)))?;

    let mut active: AUploadSession = session.into();
    active.missing = Set(missing_value);
    active.update(&state.web_db).await?;

    Ok(ok_json(BlobsResponse {
        uploaded,
        remaining,
    }))
}

fn is_unique_violation(err: &DbErr) -> bool {
    let sqlx_err = match err {
        DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e)) => e,
        _ => return false,
    };
    matches!(
        sqlx_err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}
