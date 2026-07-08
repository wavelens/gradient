/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::CachePermission;
use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use gradient_core::ServerState;
use gradient_proto::ingest::{IngestInput, SignTargets, ingest_nar};
use gradient_storage::PartialStore;
use gradient_types::*;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize)]
pub struct NarinfoPart {
    store_path: String,
    file_hash: String,
    file_size: i64,
    nar_size: i64,
    nar_hash: String,
    #[serde(default)]
    references: Vec<String>,
    #[serde(default)]
    deriver: Option<String>,
}

pub async fn nars_upload(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache_name): Path<String>,
    mut multipart: Multipart,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        },
    )
    .await?;

    let mut narinfo: Option<NarinfoPart> = None;
    let mut nar_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::BadRequest(ErrorCode::INPUT_VALIDATION, e.to_string()))?
    {
        match field.name() {
            Some("narinfo") => {
                let text = field.text().await.map_err(|e| {
                    WebError::BadRequest(ErrorCode::INPUT_VALIDATION, e.to_string())
                })?;
                let parsed: NarinfoPart = serde_json::from_str(&text).map_err(|e| {
                    WebError::BadRequest(
                        ErrorCode::INPUT_VALIDATION,
                        format!("invalid narinfo JSON: {e}"),
                    )
                })?;
                narinfo = Some(parsed);
            }
            Some("nar") => {
                let bytes = field.bytes().await.map_err(|e| {
                    WebError::BadRequest(ErrorCode::INPUT_VALIDATION, e.to_string())
                })?;
                nar_bytes = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    let narinfo = narinfo.ok_or_else(|| {
        WebError::BadRequest(ErrorCode::INPUT_VALIDATION, "missing narinfo part".into())
    })?;
    let nar_bytes = nar_bytes.ok_or_else(|| {
        WebError::BadRequest(ErrorCode::INPUT_VALIDATION, "missing nar part".into())
    })?;

    if nar_bytes.len() as i64 != narinfo.file_size {
        return Err(WebError::BadRequest(
            ErrorCode::INPUT_VALIDATION,
            format!(
                "nar size mismatch: declared {} bytes, got {}",
                narinfo.file_size,
                nar_bytes.len()
            ),
        ));
    }
    if let Err(e) =
        gradient_storage::verify_nar_bytes(&nar_bytes, &narinfo.file_hash, narinfo.file_size as u64)
    {
        return Err(WebError::BadRequest(
            ErrorCode::INPUT_VALIDATION,
            format!("nar content verification failed: {e}"),
        ));
    }

    let outcome = ingest_nar(
        &state.web_db,
        &state.nar_storage,
        nar_bytes,
        IngestInput {
            store_path: &narinfo.store_path,
            file_hash: &narinfo.file_hash,
            file_size: narinfo.file_size,
            nar_size: narinfo.nar_size,
            nar_hash: &narinfo.nar_hash,
            references: &narinfo.references,
            deriver: narinfo.deriver.as_deref(),
        },
        SignTargets::Cache(cache.id),
    )
    .await
    .map_err(WebError::from)?;

    sign_uploaded_path(&state, &narinfo, outcome.cached_path).await;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_NAR_UPLOAD,
        &info,
        Some(json!({
            "cache_id": cache.id.to_string(),
            "cache_name": cache.name,
            "store_path": narinfo.store_path,
            "created": outcome.created,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(json!({
            "store_path": narinfo.store_path,
            "created": outcome.created,
        })),
    ))
}

/// Sign the freshly ingested path in place so its narinfo is servable
/// immediately, rather than lagging until the periodic sweep runs.
async fn sign_uploaded_path(
    state: &ServerState,
    narinfo: &NarinfoPart,
    cached_path: gradient_types::ids::CachedPathId,
) {
    gradient_proto::signing::sign_cached_path(
        &state.web_db,
        &state.config.secrets.crypt_secret_file,
        &state.config.server.serve_url,
        gradient_proto::signing::SignRequest {
            cached_path,
            store_path: &narinfo.store_path,
            nar_hash: &narinfo.nar_hash,
            nar_size: narinfo.nar_size,
            references: &narinfo.references,
        },
    )
    .await;
}

#[derive(Deserialize)]
pub struct ChunkQuery {
    offset: u64,
}

/// Disk-staging store for chunked uploads. Reuses the `#225` `PartialStore` but
/// under a dedicated root so per-NAR keys never collide with the proto path's
/// per-session budget accounting.
fn upload_partial_store(state: &ServerState) -> WebResult<PartialStore> {
    let root = format!("{}/nar-upload-partial", state.config.storage.base_path);
    let ttl = Duration::from_secs(state.config.proto.nar_partial_ttl_secs);
    Ok(PartialStore::new(root, ttl)?)
}

/// A store path's base name (`<hash>-<name>`) used as the staging key. Rejected
/// when it could escape the staging root.
fn require_safe_hash(store_hash: &str) -> WebResult<()> {
    if store_hash.is_empty() || store_hash.contains('/') || store_hash.contains("..") {
        return Err(WebError::BadRequest(
            ErrorCode::INPUT_VALIDATION,
            "invalid store hash".into(),
        ));
    }

    Ok(())
}

/// `PUT /caches/{cache}/nars/{store_hash}/chunk?offset=N` - append one NAR slice
/// to the staged `.partial`. `offset` must equal the bytes already received
/// (`0` starts fresh); a mismatch returns `409` with the authoritative
/// `received` so the client can resume. Keeps each request small enough to clear
/// the reverse proxy's body limit no matter how large the NAR is.
pub async fn nar_chunk(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, store_hash)): Path<(String, String)>,
    Query(ChunkQuery { offset }): Query<ChunkQuery>,
    body: Bytes,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        },
    )
    .await?;
    require_safe_hash(&store_hash)?;

    let max = state.config.limits.max_nar_upload_size as u64;
    if offset + body.len() as u64 > max {
        return Err(WebError::PayloadTooLarge(
            ErrorCode::PAYLOAD_TOO_LARGE,
            format!("nar exceeds max upload size of {max} bytes"),
        ));
    }

    let store = upload_partial_store(&state)?;
    let key = format!("{}/{store_hash}", cache.id);
    // A fresh upload sweeps abandoned partials (best-effort); the `offset == 0`
    // append then truncates any stale prefix so a re-run restarts cleanly.
    if offset == 0 {
        let _ = store.gc().await;
    }

    // `received` is authoritative: when the caller's `offset` is contiguous we
    // append and advance it; otherwise we append nothing and report the current
    // length so the caller resyncs and resends from there.
    let staged = store.received_len(&key, &store_hash).await?;
    let received = if offset == 0 || offset == staged {
        store.append(&key, &store_hash, offset, &body).await?;
        offset + body.len() as u64
    } else {
        staged
    };

    Ok((StatusCode::OK, ok_json(json!({ "received": received }))))
}

/// `POST /caches/{cache}/nars/{store_hash}/finalize` - validate the fully staged
/// NAR against its narinfo and ingest it, then drop the partial.
pub async fn nar_finalize(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, store_hash)): Path<(String, String)>,
    axum::Json(narinfo): axum::Json<NarinfoPart>,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        },
    )
    .await?;
    require_safe_hash(&store_hash)?;

    let store = upload_partial_store(&state)?;
    let key = format!("{}/{store_hash}", cache.id);
    let staged = store.staged_len(&key).await;
    if staged as i64 != narinfo.file_size {
        return Err(WebError::BadRequest(
            ErrorCode::INPUT_VALIDATION,
            format!(
                "nar size mismatch: declared {} bytes, staged {staged}",
                narinfo.file_size
            ),
        ));
    }

    let nar_bytes = store.read_all(&key).await?;
    if let Err(e) =
        gradient_storage::verify_nar_bytes(&nar_bytes, &narinfo.file_hash, narinfo.file_size as u64)
    {
        return Err(WebError::BadRequest(
            ErrorCode::INPUT_VALIDATION,
            format!("nar content verification failed: {e}"),
        ));
    }
    let outcome = ingest_nar(
        &state.web_db,
        &state.nar_storage,
        nar_bytes,
        IngestInput {
            store_path: &narinfo.store_path,
            file_hash: &narinfo.file_hash,
            file_size: narinfo.file_size,
            nar_size: narinfo.nar_size,
            nar_hash: &narinfo.nar_hash,
            references: &narinfo.references,
            deriver: narinfo.deriver.as_deref(),
        },
        SignTargets::Cache(cache.id),
    )
    .await
    .map_err(WebError::from)?;
    let _ = store.discard(&key).await;

    sign_uploaded_path(&state, &narinfo, outcome.cached_path).await;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_NAR_UPLOAD,
        &info,
        Some(json!({
            "cache_id": cache.id.to_string(),
            "cache_name": cache.name,
            "store_path": narinfo.store_path,
            "created": outcome.created,
            "chunked": true,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(json!({
            "store_path": narinfo.store_path,
            "created": outcome.created,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::require_safe_hash;

    #[test]
    fn accepts_a_normal_store_basename() {
        assert!(require_safe_hash("bnq5n76hrfr50l5s2hbbg9vw32fvcrbc-linux-rpi-6.12.75").is_ok());
    }

    #[test]
    fn rejects_traversal_and_separators() {
        for bad in ["", "../etc/passwd", "a/b", "..", "x/.."] {
            assert!(require_safe_hash(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
