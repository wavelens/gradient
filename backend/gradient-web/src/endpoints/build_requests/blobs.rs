/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildRequestRefusal;
use crate::access::has_permission;
use crate::api_spec::Binary;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, is_unique_violation};
use crate::helpers::ok_json;
use crate::lookup::require;
use crate::permissions::Permission;
use crate::resource::Resource;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_storage::NarStore;
use gradient_types::ids::{BuildRequestBlobId, ProjectId, UploadSessionId};
use gradient_types::input::decode_blake3_hex;
use gradient_types::{BaseResponse, EUploadSession, MBuildRequestBlob, MUser, now};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BlobsResponse {
    pub uploaded: usize,
    pub remaining: usize,
}

struct VerifiedBlob {
    name: String,
    hash: [u8; 32],
    data: Vec<u8>,
}

#[utoipa::path(
    post,
    path = "/build-requests/{session}/blobs",
    operation_id = "postBuildRequestBlobs",
    tag = "build-requests",
    summary = "Upload missing blobs for an upload session",
    description = "Step 2. Sends one or more blobs as multipart parts. Each part's\n**field name** is the 64-character lowercase hex BLAKE3 hash, and the\npart body is the raw file bytes. The server verifies each blob's hash,\nrejects any hash that isn't in the session's `missing` set, persists\naccepted blobs under `nar_storage`, and removes them from `missing`.\n\nThe route's body limit is `MAX_BUILD_REQUEST_SIZE` (20 MiB).",
    params(("session" = UploadSessionId, Path)),
    request_body(content = std::collections::BTreeMap<String, Binary>, content_type = "multipart/form-data"),
    responses((status = OK, description = "Blobs accepted", body = BlobsResponse), WebError)
)]
pub async fn post_blobs(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(session_id): Path<UploadSessionId>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<BlobsResponse>>> {
    let session = require::<EUploadSession>(&state.web_db, session_id).await?;

    if session.dispatched_at.is_some() {
        return Err(BuildRequestRefusal::UploadSessionAlreadyDispatched.into());
    }

    if now() > session.expires_at {
        return Err(BuildRequestRefusal::UploadSessionExpired.into());
    }

    let api_key_ref = api_key.as_ref();
    if !has_permission(
        &state.web_db,
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
        serde_json::from_value(session.missing.clone()).context("corrupt session.missing JSON")?;

    let missing_before = missing.len();
    while let Some(field) =
        multipart
            .next_field()
            .await
            .map_err(|e| BuildRequestRefusal::InvalidMultipartPayload {
                reason: e.to_string(),
            })?
    {
        let blob = verified_blob(field, &missing).await?;
        missing.remove(&blob.name);
        store_blob(&state.web_db, &state.nar_storage, session.project, blob).await?;
    }

    let remaining = missing.len();
    let missing_value = serde_json::to_value(missing.into_iter().collect::<Vec<String>>())
        .context("serialise missing list")?;

    gradient_db::upload_sessions::set_missing(&state.web_db, session, missing_value).await?;

    Ok(ok_json(BlobsResponse {
        uploaded: missing_before - remaining,
        remaining,
    }))
}

async fn verified_blob(field: Field<'_>, missing: &HashSet<String>) -> WebResult<VerifiedBlob> {
    let name = field
        .name()
        .ok_or(BuildRequestRefusal::MissingFieldName)?
        .to_string();

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&decode_blake3_hex(&name)?);
    if !missing.contains(&name) {
        return Err(BuildRequestRefusal::BlobNotExpected { name }.into());
    }

    let data = field
        .bytes()
        .await
        .map_err(|e| BuildRequestRefusal::UnreadableFieldBytes {
            reason: e.to_string(),
        })?;

    if blake3::hash(&data).as_bytes() != &hash {
        return Err(BuildRequestRefusal::BlobHashMismatch { name }.into());
    }

    Ok(VerifiedBlob {
        name,
        hash,
        data: data.to_vec(),
    })
}

async fn store_blob(
    db: &WebDb,
    nar_storage: &NarStore,
    project: ProjectId,
    blob: VerifiedBlob,
) -> WebResult<()> {
    let size = blob.data.len() as i64;
    nar_storage
        .put_blob(project.into_inner(), &blob.hash, blob.data)
        .await
        .context("persist blob")?;

    let stored_at = now();
    let inserted = gradient_db::build_request_blobs::insert(
        db,
        MBuildRequestBlob {
            id: BuildRequestBlobId::now_v7(),
            project,
            hash: blob.hash.to_vec(),
            size,
            created_at: stored_at,
            last_used_at: stored_at,
        },
    )
    .await;

    match inserted {
        Err(err) if !is_unique_violation(&err) => Err(err.into()),
        _ => Ok(()),
    }
}
