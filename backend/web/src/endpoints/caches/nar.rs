/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::require_cache_auth;
use crate::error::WebError;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
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
) -> Result<Response, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e.to_string(),
            }),
        )
    })?;

    if !(path.ends_with(".nar") || path.contains(".nar.")) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Invalid path".to_string(),
            }),
        ));
    }

    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.active {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Cache is disabled".to_string(),
            }),
        ));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    // The URL uses the file hash (nix32 of compressed content).
    // Resolve it to the store hash so we can locate the on-disk NAR or pack path.
    let effective_hash = {
        let by_file = EDerivationOutput::find()
            .filter(
                Condition::all()
                    .add(CDerivationOutput::IsCached.eq(true))
                    .add(CDerivationOutput::FileHash.eq(format!("sha256:{}", path_hash))),
            )
            .one(&state.db)
            .await
            .map_err(WebError::from)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Database error: {}", e),
                    }),
                )
            })?;
        if let Some(output) = by_file {
            output.hash
        } else {
            // Fallback: path_hash may itself be a store hash (legacy / direct hash URLs).
            path_hash.clone()
        }
    };

    let compressed = match state.nar_storage.get(&effective_hash).await {
        Ok(Some(data)) => data,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Path not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to read NAR: {}", e),
                }),
            ));
        }
    };

    let bytes_len = compressed.len() as i64;
    let cache_id = cache.id;
    let state_for_metric = Arc::clone(&state.0);
    tokio::spawn(async move {
        super::super::stats::record_nar_traffic(state_for_metric, cache_id, bytes_len).await;
    });

    // Update last_fetched_at on the cache_derivation row for this (cache, derivation) pair.
    {
        let state_for_fetch = Arc::clone(&state.0);
        let hash = effective_hash.clone();
        let cache_id = cache.id;
        tokio::spawn(async move {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            let now = chrono::Utc::now().naive_utc();
            let _ = state_for_fetch
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE cache_derivation SET last_fetched_at = $1 \
                     WHERE cache = $2 AND derivation IN ( \
                         SELECT derivation FROM derivation_output WHERE hash = $3 AND is_cached = true \
                     )",
                    [
                        sea_orm::Value::ChronoDateTimeUtc(Some(Box::new(chrono::DateTime::from_naive_utc_and_offset(now, chrono::Utc)))),
                        sea_orm::Value::Uuid(Some(Box::new(cache_id))),
                        sea_orm::Value::String(Some(Box::new(hash))),
                    ],
                ))
                .await;
        });
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .body(Body::from(compressed))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to build response: {}", e),
                }),
            )
        })
}

pub async fn upstream_nar(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache_name, upstream_id, path)): Path<(String, Uuid, String)>,
) -> Result<Response, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match ECache::find()
        .filter(CCache::Name.eq(cache_name))
        .one(&state.db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    if !cache.active {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Cache is disabled".to_string(),
            }),
        ));
    }

    require_cache_auth(&headers, &state, &cache).await?;

    let upstream = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Upstream not found".to_string(),
                }),
            )
        })?;

    let base_url = upstream.url.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Not an external upstream".to_string(),
            }),
        )
    })?;

    let nar_url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let http_client = reqwest::Client::new();
    let resp = http_client.get(&nar_url).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(BaseResponse {
                error: true,
                message: format!("Upstream request failed: {}", e),
            }),
        )
    })?;

    if !resp.status().is_success() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Not found in upstream".to_string(),
            }),
        ));
    }

    let bytes = resp.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to read upstream response: {}", e),
            }),
        )
    })?;

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .body(Body::from(bytes))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to build response: {}", e),
                }),
            )
        })
}
