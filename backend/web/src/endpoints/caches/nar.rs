/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::CacheContext;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use core::sources::get_hash_from_url;
use core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

pub async fn nar(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> WebResult<Response> {
    let path_hash =
        get_hash_from_url(path.clone()).map_err(|e| WebError::BadRequest(e.to_string()))?;

    if !(path.ends_with(".nar") || path.contains(".nar.")) {
        return Err(WebError::not_found("Path"));
    }

    let ctx = CacheContext::load(&state, &headers, cache).await?;

    // The URL uses the file hash (nix32 of compressed content).
    // Resolve it to the store hash so we can locate the on-disk NAR or pack path.
    let effective_hash = resolve_effective_hash(&state, &path_hash).await?;

    let compressed = state
        .nar_storage
        .get(&effective_hash)
        .await
        .map_err(|e| WebError::InternalServerError(format!("Failed to read NAR: {}", e)))?
        .ok_or_else(|| WebError::not_found("Path"))?;

    spawn_nar_traffic_metric(Arc::clone(&state), ctx.cache.id, compressed.len() as i64);
    spawn_cache_derivation_fetch_update(Arc::clone(&state), ctx.cache.id, effective_hash);

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .body(Body::from(compressed))
        .map_err(|e| WebError::InternalServerError(format!("Failed to build response: {}", e)))
}

pub async fn upstream_nar(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache_name, upstream_id, path)): Path<(String, Uuid, String)>,
) -> WebResult<Response> {
    let ctx = CacheContext::load(&state, &headers, cache_name).await?;

    let upstream = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(ctx.cache.id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Upstream"))?;

    let base_url = upstream
        .url
        .ok_or_else(|| WebError::BadRequest("Not an external upstream".to_string()))?;

    let bytes = fetch_upstream_nar(&base_url, &path).await?;

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .body(Body::from(bytes))
        .map_err(|e| WebError::InternalServerError(format!("Failed to build response: {}", e)))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve the file hash from the URL to the store hash stored in `derivation_output`.
/// Falls back to using the path hash directly for legacy/direct-hash URLs.
async fn resolve_effective_hash(state: &Arc<ServerState>, path_hash: &str) -> WebResult<String> {
    let by_file = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::FileHash.eq(format!("sha256:{}", path_hash))),
        )
        .one(&state.db)
        .await?;

    Ok(match by_file {
        Some(output) => output.hash,
        None => path_hash.to_string(),
    })
}

fn spawn_nar_traffic_metric(state: Arc<ServerState>, cache_id: Uuid, bytes_len: i64) {
    tokio::spawn(async move {
        super::super::stats::record_nar_traffic(state, cache_id, bytes_len).await;
    });
}

fn spawn_cache_derivation_fetch_update(state: Arc<ServerState>, cache_id: Uuid, hash: String) {
    tokio::spawn(async move {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
        let now = chrono::Utc::now().naive_utc();
        let _ = state
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cache_derivation SET last_fetched_at = $1 \
                 WHERE cache = $2 AND derivation IN ( \
                     SELECT derivation FROM derivation_output WHERE hash = $3 AND is_cached = true \
                 )",
                [
                    sea_orm::Value::ChronoDateTimeUtc(Some(Box::new(
                        chrono::DateTime::from_naive_utc_and_offset(now, chrono::Utc),
                    ))),
                    sea_orm::Value::Uuid(Some(Box::new(cache_id))),
                    sea_orm::Value::String(Some(Box::new(hash))),
                ],
            ))
            .await;
    });
}

async fn fetch_upstream_nar(base_url: &str, path: &str) -> WebResult<bytes::Bytes> {
    let nar_url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let resp = reqwest::Client::new()
        .get(&nar_url)
        .send()
        .await
        .map_err(|e| WebError::InternalServerError(format!("Upstream request failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err(WebError::not_found("NAR in upstream"));
    }

    resp.bytes().await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to read upstream response: {}", e))
    })
}
