/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::types::*;
use core::input::check_index_name;
use core::database::get_cache_by_name;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct NixCacheInfo {
    #[serde(rename = "WantMassQuery")]
    want_mass_query: bool,
    #[serde(rename = "StoreDir")]
    store_dir: String,
    #[serde(rename = "Priority")]
    priority: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let caches = ECache::find()
        .filter(CCache::CreatedBy.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let caches: ListResponse = caches
        .iter()
        .map(|c| ListItem {
            id: c.id,
            name: c.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: caches,
    };

    Ok(Json(res))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeCacheRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Cache Name".to_string(),
            }),
        ));
    }

    let cache = get_cache_by_name(state.0.clone(), user.id, body.name.clone()).await;

    if cache.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Cache Name already exists".to_string(),
            }),
        ));
    }

    let cache = ACache {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        priority: Set(body.priority),
        // TODO: Generate signing key
        signing_key: Set("".to_string()),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let cache = cache.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: cache.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<MCache>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache =
        match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
            Some(c) => c,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Cache not found".to_string(),
                    }),
                ))
            }
        };

    let res = BaseResponse {
        error: false,
        message: cache,
    };

    Ok(Json(res))
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache =
        match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
            Some(c) => c,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Cache not found".to_string(),
                    }),
                ))
            }
        };

    let acache: ACache = cache.into();
    acache.delete(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Cache deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache =
        match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
            Some(c) => c,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Cache not found".to_string(),
                    }),
                ))
            }
        };

    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    acache.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Cache enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache =
        match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
            Some(c) => c,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Cache not found".to_string(),
                    }),
                ))
            }
        };

    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    acache.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Cache disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn nix_cache_info() -> Result<Json<NixCacheInfo>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: 0,
    };

    Ok(Json(res))
}

pub async fn path(
    _state: State<Arc<ServerState>>,
    Path((_cache, _path)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

pub async fn nar(
    _state: State<Arc<ServerState>>,
    Path((_cache, _path)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

