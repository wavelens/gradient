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
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter};
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

/// Resolve the file hash from the URL to the store hash used as the NAR
/// storage key. Checks `derivation_output` first (build outputs), then
/// `cached_path` (for standalone store paths such as `.drv` files).
/// Falls back to the URL hash for legacy/direct-hash URLs.
async fn resolve_effective_hash(state: &Arc<ServerState>, path_hash: &str) -> WebResult<String> {
    resolve_effective_hash_db(&state.db, path_hash).await
}

pub(crate) async fn resolve_effective_hash_db(
    db: &DatabaseConnection,
    path_hash: &str,
) -> WebResult<String> {
    let file_hash_prefixed = format!("sha256:{}", path_hash);

    let by_file = EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::FileHash.eq(&file_hash_prefixed)),
        )
        .one(db)
        .await?;

    if let Some(output) = by_file {
        return Ok(output.hash);
    }

    let by_cached_path = ECachedPath::find()
        .filter(CCachedPath::FileHash.eq(&file_hash_prefixed))
        .one(db)
        .await?;

    if let Some(row) = by_cached_path {
        return Ok(row.hash);
    }

    Ok(path_hash.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};

    // Placeholder file hash (nix32 52-char) as it appears in a narinfo URL.
    const FILE_HASH_NIX32: &str = "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
    const STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn empty_output_page() -> Vec<entity::derivation_output::Model> {
        Vec::new()
    }

    fn cached_path_row() -> entity::cached_path::Model {
        entity::cached_path::Model {
            id: uuid::Uuid::new_v4(),
            store_path: format!("/nix/store/{STORE_HASH}-hello.drv"),
            hash: STORE_HASH.to_string(),
            package: "hello.drv".to_string(),
            file_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
            file_size: Some(1234),
            nar_size: Some(2048),
            nar_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
            references: Some(String::new()),
            ca: None,
            created_at: Utc::now().naive_utc(),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// When no derivation_output matches but a cached_path does, the
    /// resolver returns the cached_path's store hash — this is the key
    /// the NAR blob was written under by `pack_store_path`.
    #[test]
    fn resolve_falls_back_to_cached_path_for_drv() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty_output_page()])
            .append_query_results([vec![cached_path_row()]])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, STORE_HASH);
    }

    /// When neither table has a match, the URL hash is returned unchanged
    /// (legacy/direct-hash URL behaviour preserved).
    #[test]
    fn resolve_falls_back_to_url_hash_when_no_match() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty_output_page()])
            .append_query_results([Vec::<entity::cached_path::Model>::new()])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, FILE_HASH_NIX32);
    }
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
