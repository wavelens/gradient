/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{get_nar_by_hash, require_cache_auth};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use core::sources::get_hash_from_url;
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: cache.priority,
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/x-nix-cache-info"),
        )
        .body(res.to_nix_string())
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

pub async fn gradient_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let body = format!(
        "GradientVersion: {}\nGradientUrl: {}\n",
        env!("CARGO_PKG_VERSION"),
        state.cli.serve_url,
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/x-gradient-cache-info"),
        )
        .body(body)
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

pub async fn path(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> Result<Response<String>, (StatusCode, Json<BaseResponse<String>>)> {
    let path_hash = get_hash_from_url(path.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: e.to_string(),
            }),
        )
    })?;

    if !path.ends_with(".narinfo") {
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

    if let Ok(path_info) =
        get_nar_by_hash(Arc::clone(&state), cache.clone(), path_hash.clone()).await
    {
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/x-nix-narinfo"),
            )
            .body(path_info.to_nix_string())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to build response: {}", e),
                    }),
                )
            });
    }

    // Fall back: check external upstream caches.
    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.db)
        .await
        .unwrap_or_default();

    let http_client = reqwest::Client::new();
    for upstream in upstreams {
        let Some(ref base_url) = upstream.url else {
            continue;
        };
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), path_hash);
        let Ok(resp) = http_client.get(&narinfo_url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.text().await else {
            continue;
        };
        // Rewrite the URL: field to proxy through our upstream_nar endpoint.
        let rewritten = body
            .lines()
            .map(|line| {
                if let Some(nar_path) = line.strip_prefix("URL: ") {
                    format!("URL: nar/upstream/{}/{}", upstream.id, nar_path.trim())
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/x-nix-narinfo"),
            )
            .body(rewritten)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Failed to build response: {}", e),
                    }),
                )
            });
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(BaseResponse {
            error: true,
            message: "Path not found".to_string(),
        }),
    ))
}
