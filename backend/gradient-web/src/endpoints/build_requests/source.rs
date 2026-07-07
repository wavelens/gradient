/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/source` - accepts a pre-packed source NAR (multipart
//! fields `nar`, `target`, `system`), computes the `/nix/store/<hash>-source`
//! path server-side, and finalises a build-request evaluation. The `nix`-feature
//! CLI uses this to skip the per-file blob manifest.

use super::dispatch::{DispatchResponse, finalize_build_request};
use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use gradient_core::ServerState;
use gradient_storage::PartialStore;
use gradient_storage::source_nar::source_nar_from_bytes;
use gradient_types::{BaseResponse, MUser};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize)]
pub struct SourceQuery {
    pub organization: String,
}

pub async fn post_source(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Query(query): Query<SourceQuery>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let org = load_org(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        query.organization,
        OrgAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let mut nar_bytes: Option<Vec<u8>> = None;
    let mut target: Option<String> = None;
    let mut system: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::bad_request(format!("Invalid multipart payload: {}", e)))?
    {
        match field.name() {
            Some("nar") => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| WebError::bad_request(format!("Failed to read nar: {}", e)))?;
                nar_bytes = Some(data.to_vec());
            }
            Some("target") => target = field.text().await.ok(),
            Some("system") => system = field.text().await.ok(),
            _ => {}
        }
    }

    let nar_bytes = nar_bytes.ok_or_else(|| WebError::bad_request("missing `nar` field"))?;
    if nar_bytes.is_empty() {
        return Err(WebError::bad_request("empty `nar` field"));
    }

    let nar = source_nar_from_bytes(nar_bytes)
        .await
        .map_err(|e| WebError::internal(format!("Failed to read source NAR: {}", e)))?;

    let response = finalize_build_request(&state, org.id, &user, &nar, target, system).await?;

    Ok(ok_json(response))
}

#[derive(Deserialize)]
pub struct ChunkQuery {
    offset: u64,
}

#[derive(Deserialize)]
pub struct SourceFinalize {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
}

/// Disk-staging store for chunked source uploads. Its own root keeps the
/// per-user upload keys away from the cache NAR partials.
fn source_partial_store(state: &ServerState) -> WebResult<PartialStore> {
    let root = format!("{}/source-upload-partial", state.config.storage.base_path);
    let ttl = Duration::from_secs(state.config.proto.nar_partial_ttl_secs);
    Ok(PartialStore::new(root, ttl)?)
}

/// Client-chosen upload id used as the staging key; rejected when it could
/// escape the staging root.
fn require_safe_upload_id(upload: &str) -> WebResult<()> {
    if upload.is_empty() || upload.contains('/') || upload.contains("..") {
        return Err(WebError::bad_request("invalid upload id"));
    }
    Ok(())
}

/// `PUT /build-requests/source/{upload}/chunk?offset=N` - append one slice of the
/// source NAR to the caller's staged `.partial`. `offset` must equal the bytes
/// already received (`0` starts fresh); a mismatch appends nothing and returns
/// the authoritative `received` so the client resumes. Keeps each request small
/// enough to clear the reverse proxy's body limit regardless of source size.
pub async fn source_chunk(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(upload): Path<String>,
    Query(ChunkQuery { offset }): Query<ChunkQuery>,
    body: Bytes,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    require_safe_upload_id(&upload)?;

    let max = state.config.limits.max_source_upload_size as u64;
    if offset + body.len() as u64 > max {
        return Err(WebError::payload_too_large(format!(
            "source upload exceeds max size of {max} bytes"
        )));
    }

    let store = source_partial_store(&state)?;
    let key = format!("{}/{upload}", user.id);
    if offset == 0 {
        let _ = store.gc();
    }

    let staged = store.received_len(&key, &upload)?;
    let received = if offset == 0 || offset == staged {
        store.append(&key, &upload, offset, &body)?;
        offset + body.len() as u64
    } else {
        staged
    };

    Ok(ok_json(json!({ "received": received })))
}

/// `POST /build-requests/source/{upload}/finalize?organization=X` - reassemble the
/// staged NAR, compute its `/nix/store/<hash>-source` path, and finalise the
/// build-request evaluation, then drop the partial.
pub async fn source_finalize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(upload): Path<String>,
    Query(query): Query<SourceQuery>,
    Json(body): Json<SourceFinalize>,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    require_safe_upload_id(&upload)?;

    let org = load_org(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        query.organization,
        OrgAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let store = source_partial_store(&state)?;
    let key = format!("{}/{upload}", user.id);
    let nar_bytes = store
        .read_all(&key)
        .map_err(|e| WebError::bad_request(format!("no staged source upload: {e}")))?;
    if nar_bytes.is_empty() {
        let _ = store.discard(&key);
        return Err(WebError::bad_request("empty source upload"));
    }

    let nar = source_nar_from_bytes(nar_bytes)
        .await
        .map_err(|e| WebError::internal(format!("Failed to read source NAR: {}", e)))?;

    let response =
        finalize_build_request(&state, org.id, &user, &nar, body.target, body.system).await?;
    let _ = store.discard(&key);

    Ok(ok_json(response))
}

#[cfg(test)]
mod tests {
    use super::require_safe_upload_id;

    #[test]
    fn upload_id_rejects_path_traversal() {
        assert!(require_safe_upload_id("a1b2c3deadbeef").is_ok());
        assert!(require_safe_upload_id("").is_err());
        assert!(require_safe_upload_id("a/b").is_err());
        assert!(require_safe_upload_id("..").is_err());
        assert!(require_safe_upload_id("../../etc/passwd").is_err());
    }
}
