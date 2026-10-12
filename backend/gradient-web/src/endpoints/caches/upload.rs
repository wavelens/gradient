/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::admit::{admit, admit_nar};
use super::refusal::CacheRefusal;
use crate::api_spec::{Binary, CachePath};
use crate::audit::Audit;
use crate::endpoints::ChunkReceived;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Requires, can};
use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use gradient_core::ServerState;
use gradient_proto::import::{ImportInput, SignTargets, import_nar_reader};
use gradient_storage::PartialStore;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::events::cache::NarSigned;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub struct StoredNar {
    pub store_path: String,
    pub created: bool,
}

#[derive(utoipa::ToSchema)]
pub struct NarUploadForm {
    pub narinfo: NarinfoPart,
    #[schema(value_type = String, format = Binary)]
    pub nar: Vec<u8>,
}

#[derive(Deserialize, utoipa::ToSchema)]
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

#[utoipa::path(
    post,
    path = "/caches/{cache}/nars",
    operation_id = "uploadCacheNar",
    tag = "caches",
    summary = "Upload a NAR to a cache",
    description = "Uploads a single NAR (its bytes plus narinfo metadata) to this cache.\nRequires `writeStore`. The server validates the byte length against the\ndeclared `file_size`, stores the blob, records the cached path, and\nenqueues a signature for this cache. Re-uploading an existing store path\nreuses the cached path and only adds this cache's signature.\n\nThe route's body limit is `GRADIENT_NAR_MAX_UPLOAD_SIZE` (default 512 MiB).\nThe bundled reverse proxy additionally caps each request body at 100 MiB,\nso NARs above that must use the chunked endpoints\n(`PUT .../nars/{hash}/chunk` + `POST .../nars/{hash}/finalize`).",
    params(CachePath),
    request_body(content = NarUploadForm, content_type = "multipart/form-data"),
    responses((status = CREATED, description = "NAR stored", body = StoredNar), WebError)
)]
pub async fn nars_upload(
    CacheScope { cache, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult<impl IntoResponse> {
    let max = u64::try_from(state.config.nar.max_upload_size.get()).unwrap_or(u64::MAX);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(max);
    let permit = admit(
        &state.config,
        &state.memory.upload_admission,
        declared.min(max),
    )
    .await?;
    let upload_store = upload_partial_store(&state.config)?;
    let stage_key = format!("{}/oneshot-{}", cache.id, uuid::Uuid::now_v7());

    let mut narinfo: Option<NarinfoPart> = None;
    let mut staged_size: Option<u64> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?
    {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("narinfo") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?;
                let parsed: NarinfoPart =
                    serde_json::from_str(&text).map_err(|e| CacheRefusal::InvalidNarinfoJson {
                        reason: e.to_string(),
                    })?;
                narinfo = Some(parsed);
            }
            Some("nar") => {
                staged_size = Some(stage_nar_field(&upload_store, &stage_key, field, max).await?);
            }
            _ => {}
        }
    }

    let narinfo = match narinfo {
        Some(n) => n,
        None => {
            let _ = upload_store.discard(&stage_key).await;
            return Err(CacheRefusal::MissingPart("narinfo").into());
        }
    };
    let staged_size = staged_size.ok_or(CacheRefusal::MissingPart("nar"))?;

    if staged_size as i64 != narinfo.file_size {
        let _ = upload_store.discard(&stage_key).await;
        return Err(CacheRefusal::UploadedNarSizeMismatch {
            declared: narinfo.file_size,
            got: staged_size,
        }
        .into());
    }

    let verify_reader = upload_store
        .open_read(&stage_key)
        .await
        .context("read staged NAR")?;
    if let Err(e) = gradient_storage::verify_nar_reader(
        verify_reader,
        &narinfo.file_hash,
        narinfo.file_size as u64,
    )
    .await
    {
        let _ = upload_store.discard(&stage_key).await;
        return Err(CacheRefusal::NarVerificationFailed {
            reason: e.to_string(),
        }
        .into());
    }

    permit.committed();
    let claim = admit_nar(
        &state.config,
        &state.memory.upload_admission,
        &narinfo_hash(&narinfo.store_path),
        narinfo.file_size as u64,
    )
    .await?;
    let nar_reader = upload_store
        .open_read(&stage_key)
        .await
        .context("read staged NAR")?;
    let outcome = import_nar_reader(
        &state.web_db,
        &state.nar_storage,
        &state.graph,
        nar_reader,
        ImportInput {
            store_path: &narinfo.store_path,
            file_hash: &narinfo.file_hash,
            file_size: narinfo.file_size,
            nar_size: narinfo.nar_size,
            nar_hash: &narinfo.nar_hash,
            references: &narinfo.references,
            deriver: narinfo.deriver.as_deref(),
            ca: None,
        },
        SignTargets::Cache(cache.id),
    )
    .await
    .map_err(WebError::from)?;
    if let Some(claim) = claim {
        claim.committed();
    }

    let _ = upload_store.discard(&stage_key).await;

    for cache in outcome.signed {
        state.events.publish(NarSigned {
            cache,
            hash: narinfo_hash(&narinfo.store_path),
        });
    }

    audit.write(Action::CacheNarUpload, EventOwner::cache(cache.id), json!({ "cache_id": cache.id.to_string(), "cache_name": cache.name, "store_path": narinfo.store_path, "created": outcome.created, }))
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(StoredNar {
            store_path: narinfo.store_path,
            created: outcome.created,
        }),
    ))
}

fn narinfo_hash(store_path: &str) -> String {
    store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()
        .unwrap_or("")
        .to_owned()
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NarChunkQuery {
    offset: u64,
}

fn upload_partial_store(config: &RuntimeConfig) -> WebResult<PartialStore> {
    Ok(PartialStore::new(config.server.nar_upload_partial_dir())?)
}

async fn stage_nar_field(
    store: &PartialStore,
    key: &str,
    mut field: axum::extract::multipart::Field<'_>,
    max: u64,
) -> WebResult<u64> {
    let token = "oneshot";
    let mut offset = 0u64;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?
    {
        if offset + chunk.len() as u64 > max {
            let _ = store.discard(key).await;
            return Err(CacheRefusal::NarTooLarge { max }.into());
        }
        store
            .append(key, token, offset, &chunk)
            .await
            .context("stage NAR")?;
        offset += chunk.len() as u64;
    }
    Ok(offset)
}

fn require_safe_hash(store_hash: &str) -> WebResult<()> {
    if store_hash.is_empty() || store_hash.contains('/') || store_hash.contains("..") {
        return Err(CacheRefusal::InvalidStoreHash.into());
    }

    Ok(())
}

#[utoipa::path(
    put,
    path = "/caches/{cache}/nars/{hash}/chunk",
    operation_id = "uploadCacheNarChunk",
    tag = "caches",
    summary = "Append a chunk of a NAR being uploaded",
    description = "Appends one slice of a NAR to a server-side `.partial` staging file,\none per `(cache, hash)`. Used to upload NARs larger than the reverse\nproxy's per-request body limit by splitting them across several requests.\nRequires `writeStore`. `offset == 0` starts fresh (truncating any stale\npartial); otherwise the chunk is appended only when `offset` equals the\nbytes already staged. The response's `received` is authoritative: the\nclient sets its next offset to it (resending if it did not advance) and\ncalls `POST .../finalize` once `received` equals the NAR size.",
    params(CachePath, ("hash" = String, Path), NarChunkQuery),
    request_body(content = Binary, content_type = "application/octet-stream"),
    responses((status = OK, description = "Chunk staged", body = ChunkReceived), WebError)
)]
pub async fn nar_chunk(
    CacheScope { cache, params, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    Query(NarChunkQuery { offset }): Query<NarChunkQuery>,
    body: Bytes,
) -> WebResult<impl IntoResponse> {
    let store_hash: String = params.param("hash")?;

    require_safe_hash(&store_hash)?;

    let max = u64::try_from(state.config.nar.max_upload_size.get()).unwrap_or(u64::MAX);
    if offset + body.len() as u64 > max {
        return Err(CacheRefusal::NarTooLarge { max }.into());
    }

    let store = upload_partial_store(&state.config)?;
    let key = format!("{}/{store_hash}", cache.id);
    // An `offset == 0` append is truncating any stale prefix. Abandoned partials are left to the
    // deep GC.

    // `received` is authoritative. A non-contiguous `offset` is appending nothing, and the current
    // length is reported for the caller to resync.
    let staged = store.received_len(&key, &store_hash).await?;
    let received = if offset == 0 || offset == staged {
        let permit = admit(
            &state.config,
            &state.memory.upload_admission,
            body.len() as u64,
        )
        .await?;
        store.append(&key, &store_hash, offset, &body).await?;
        permit.committed();
        offset + body.len() as u64
    } else {
        staged
    };

    Ok((StatusCode::OK, ok_json(ChunkReceived { received })))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/nars/{hash}/finalize",
    operation_id = "finalizeCacheNarUpload",
    tag = "caches",
    summary = "Finalize a chunked NAR upload",
    description = "Validates the fully staged NAR against its narinfo (the staged byte\nlength must equal `file_size`), imports it exactly like the single-shot\nupload, and drops the `.partial`. Requires `writeStore`.",
    params(CachePath, ("hash" = String, Path)),
    request_body = NarinfoPart,
    responses((status = CREATED, description = "NAR stored", body = StoredNar), WebError)
)]
pub async fn nar_finalize(
    CacheScope { cache, params, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    axum::Json(narinfo): axum::Json<NarinfoPart>,
) -> WebResult<impl IntoResponse> {
    let store_hash: String = params.param("hash")?;

    require_safe_hash(&store_hash)?;

    let store = upload_partial_store(&state.config)?;
    let key = format!("{}/{store_hash}", cache.id);
    let staged = store.staged_len(&key).await;
    if staged as i64 != narinfo.file_size {
        return Err(CacheRefusal::StagedNarSizeMismatch {
            declared: narinfo.file_size,
            staged,
        }
        .into());
    }

    let claim = admit_nar(
        &state.config,
        &state.memory.upload_admission,
        &narinfo_hash(&narinfo.store_path),
        narinfo.file_size as u64,
    )
    .await?;
    let verify_reader = store.open_read(&key).await.context("read staged NAR")?;
    if let Err(e) = gradient_storage::verify_nar_reader(
        verify_reader,
        &narinfo.file_hash,
        narinfo.file_size as u64,
    )
    .await
    {
        return Err(CacheRefusal::NarVerificationFailed {
            reason: e.to_string(),
        }
        .into());
    }
    let nar_reader = store.open_read(&key).await.context("read staged NAR")?;
    let outcome = import_nar_reader(
        &state.web_db,
        &state.nar_storage,
        &state.graph,
        nar_reader,
        ImportInput {
            store_path: &narinfo.store_path,
            file_hash: &narinfo.file_hash,
            file_size: narinfo.file_size,
            nar_size: narinfo.nar_size,
            nar_hash: &narinfo.nar_hash,
            references: &narinfo.references,
            deriver: narinfo.deriver.as_deref(),
            ca: None,
        },
        SignTargets::Cache(cache.id),
    )
    .await
    .map_err(WebError::from)?;
    if let Some(claim) = claim {
        claim.committed();
    }

    let _ = store.discard(&key).await;

    for cache in outcome.signed {
        state.events.publish(NarSigned {
            cache,
            hash: narinfo_hash(&narinfo.store_path),
        });
    }

    audit.write(Action::CacheNarUpload, EventOwner::cache(cache.id), json!({ "cache_id": cache.id.to_string(), "cache_name": cache.name, "store_path": narinfo.store_path, "created": outcome.created, "chunked": true, }))
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(StoredNar {
            store_path: narinfo.store_path,
            created: outcome.created,
        }),
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
