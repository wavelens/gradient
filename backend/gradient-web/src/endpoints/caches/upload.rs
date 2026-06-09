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
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use gradient_core::cache::ingest::{IngestInput, SignTargets, ingest_nar};
use gradient_core::types::*;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
struct NarinfoPart {
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
                let text = field
                    .text()
                    .await
                    .map_err(|e| WebError::BadRequest(ErrorCode::INPUT_VALIDATION, e.to_string()))?;
                let parsed: NarinfoPart = serde_json::from_str(&text).map_err(|e| {
                    WebError::BadRequest(
                        ErrorCode::INPUT_VALIDATION,
                        format!("invalid narinfo JSON: {e}"),
                    )
                })?;
                narinfo = Some(parsed);
            }
            Some("nar") => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| WebError::BadRequest(ErrorCode::INPUT_VALIDATION, e.to_string()))?;
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
