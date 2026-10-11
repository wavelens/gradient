/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::has_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use crate::resource::Resource;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use gradient_core::ServerState;
use gradient_types::ids::{BuildRequestBlobId, ProjectId, UploadSessionId};
use gradient_types::input::decode_blake3_hex;
use gradient_types::{AUploadSession, BaseResponse, EUploadSession, MBuildRequestBlob, MUser, now};
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

struct VerifiedBlob {
    name: String,
    hash: [u8; 32],
    data: Vec<u8>,
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
        .ok_or_else(|| WebError::not_found(Resource::UploadSession))?;

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
        session.project,
        Permission::TriggerEvaluation,
        api_key_ref,
    )
    .await?
    {
        return Err(WebError::not_found(Resource::UploadSession));
    }

    let mut missing: HashSet<String> =
        serde_json::from_value(session.missing.clone()).context("Corrupt session.missing JSON")?;

    let missing_before = missing.len();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::bad_request(format!("Invalid multipart payload: {}", e)))?
    {
        let blob = verified_blob(field, &missing).await?;
        missing.remove(&blob.name);
        store_blob(&state, session.project, blob).await?;
    }

    let remaining = missing.len();
    let missing_value = serde_json::to_value(missing.into_iter().collect::<Vec<String>>())
        .context("Failed to serialise missing list")?;

    let mut active: AUploadSession = session.into();
    active.missing = Set(missing_value);
    active.update(&state.web_db).await?;

    Ok(ok_json(BlobsResponse {
        uploaded: missing_before - remaining,
        remaining,
    }))
}

async fn verified_blob(field: Field<'_>, missing: &HashSet<String>) -> WebResult<VerifiedBlob> {
    let name = field
        .name()
        .ok_or_else(|| WebError::bad_request("Missing field name"))?
        .to_string();

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&decode_blake3_hex(&name)?);
    if !missing.contains(&name) {
        return Err(WebError::bad_request(format!(
            "hash not in session.missing: {}",
            name
        )));
    }

    let data = field
        .bytes()
        .await
        .map_err(|e| WebError::bad_request(format!("Failed to read field bytes: {}", e)))?;

    if blake3::hash(&data).as_bytes() != &hash {
        return Err(WebError::bad_request(format!("hash mismatch for {}", name)));
    }

    Ok(VerifiedBlob {
        name,
        hash,
        data: data.to_vec(),
    })
}

async fn store_blob(state: &ServerState, project: ProjectId, blob: VerifiedBlob) -> WebResult<()> {
    let size = blob.data.len() as i64;
    state
        .nar_storage
        .put_blob(project.into_inner(), &blob.hash, blob.data)
        .await
        .context("Failed to persist blob")?;

    let stored_at = now();
    let inserted = MBuildRequestBlob {
        id: BuildRequestBlobId::now_v7(),
        project,
        hash: blob.hash.to_vec(),
        size,
        created_at: stored_at,
        last_used_at: stored_at,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await;

    match inserted {
        Err(err) if !is_unique_violation(&err) => Err(err.into()),
        _ => Ok(()),
    }
}

fn is_unique_violation(err: &DbErr) -> bool {
    let sqlx_err = match err {
        DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e)) => {
            e.as_ref()
        }
        _ => return false,
    };

    matches!(
        sqlx_err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}
