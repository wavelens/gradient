/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::admit::{admit, admit_nar};
use super::refusal::CacheRefusal;
use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::CachePermission;
use anyhow::Context;
use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use gradient_core::ServerState;
use gradient_proto::import::{ImportInput, SignTargets, import_nar_reader};
use gradient_storage::PartialStore;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::events::cache::NarSigned;
use gradient_types::*;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

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
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult<impl IntoResponse> {
    let db = state.db();
    let cache = load_cache(
        &db,
        Caller::User(&user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        },
    )
    .await?;

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

    audit_record(
        &db,
        Some(user.id),
        Action::CacheNarUpload,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
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

fn narinfo_hash(store_path: &str) -> String {
    store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()
        .unwrap_or("")
        .to_owned()
}

#[derive(Deserialize)]
pub struct ChunkQuery {
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

pub async fn nar_chunk(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, store_hash)): Path<(String, String)>,
    Query(ChunkQuery { offset }): Query<ChunkQuery>,
    body: Bytes,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(
        &state.db(),
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

    Ok((StatusCode::OK, ok_json(json!({ "received": received }))))
}

pub async fn nar_finalize(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, store_hash)): Path<(String, String)>,
    axum::Json(narinfo): axum::Json<NarinfoPart>,
) -> WebResult<impl IntoResponse> {
    let db = state.db();
    let cache = load_cache(
        &db,
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

    audit_record(
        &db,
        Some(user.id),
        Action::CacheNarUpload,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
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
