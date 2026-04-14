/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::cleanup_nars_for_orgs;
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{NaiveDateTime, Utc};
use core::db::{get_any_cache_by_name, get_cache_by_name};
use core::sources::{format_cache_public_key, generate_signing_key};
use core::types::input::{check_index_name, validate_display_name};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
    pub public: Option<bool>,
}

#[derive(Serialize)]
pub struct CacheResponse {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub public_key: String,
    pub public: bool,
    pub created_by: Uuid,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub can_edit: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchCacheRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i32>,
}

pub async fn get_cache_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(Json(BaseResponse {
            error: false,
            message: false,
        }));
    }
    let exists = ECache::find()
        .filter(CCache::Name.eq(name.as_str()))
        .one(&state.db)
        .await?
        .is_some();
    Ok(Json(BaseResponse {
        error: false,
        message: !exists,
    }))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    // TODO: Implement pagination
    // Find all orgs the user belongs to
    let org_memberships = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.db)
        .await?;

    let org_ids: Vec<Uuid> = org_memberships
        .into_iter()
        .map(|m| m.organization)
        .collect();

    // Find cache IDs subscribed by those orgs
    let org_cache_ids: Vec<Uuid> = if org_ids.is_empty() {
        vec![]
    } else {
        EOrganizationCache::find()
            .filter(COrganizationCache::Organization.is_in(org_ids))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect()
    };

    let caches = ECache::find()
        .filter(
            Condition::any()
                .add(CCache::CreatedBy.eq(user.id))
                .add(CCache::Id.is_in(org_cache_ids)),
        )
        .all(&state.db)
        .await?;

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
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Cache Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
    }

    let existing_cache = ECache::find()
        .filter(CCache::Name.eq(body.name.clone()))
        .one(&state.db)
        .await?;

    if existing_cache.is_some() {
        return Err(WebError::already_exists("Cache Name"));
    }

    let (private_key, public_key) = generate_signing_key(state.cli.crypt_secret_file.clone())
        .map_err(|e| {
            tracing::error!("Failed to generate signing key: {}", e);
            WebError::InternalServerError("Failed to generate signing key".to_string())
        })?;

    let cache = ACache {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        priority: Set(body.priority),
        public_key: Set(public_key),
        private_key: Set(private_key),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
    };

    let cache = cache.insert(&state.db).await?;

    ACacheUpstream {
        id: Set(Uuid::new_v4()),
        cache: Set(cache.id),
        display_name: Set("cache.nixos.org".to_string()),
        mode: Set(CacheSubscriptionMode::ReadOnly),
        upstream_cache: Set(None),
        url: Set(Some("https://cache.nixos.org".to_string())),
        public_key: Set(Some(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".to_string(),
        )),
    }
    .insert(&state.db)
    .await?;

    let res = BaseResponse {
        error: false,
        message: cache.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_public_caches(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    let caches = ECache::find()
        .filter(CCache::Public.eq(true))
        .all(&state.db)
        .await?;

    Ok(Json(BaseResponse {
        error: false,
        message: caches,
    }))
}

pub async fn get_cache(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheResponse>>> {
    let cache: MCache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public {
        match &maybe_user {
            Some(user) if cache.created_by == user.id => {}
            _ => return Err(WebError::not_found("Cache")),
        }
    }

    let public_key = format_cache_public_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!("Failed to derive public key: {}", e);
        WebError::InternalServerError("Failed to derive public key".to_string())
    })?;

    let can_edit = matches!(&maybe_user, Some(u) if u.id == cache.created_by);

    let res = BaseResponse {
        error: false,
        message: CacheResponse {
            id: cache.id,
            name: cache.name,
            display_name: cache.display_name,
            description: cache.description,
            active: cache.active,
            priority: cache.priority,
            public_key,
            public: cache.public,
            created_by: cache.created_by,
            created_at: cache.created_at,
            managed: cache.managed,
            can_edit,
        },
    };

    Ok(Json(res))
}

pub async fn patch_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<PatchCacheRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
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

    // Prevent modification of state-managed caches
    if cache.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.".to_string(),
            }),
        ));
    }

    let mut acache: ACache = cache.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Cache Name".to_string(),
                }),
            ));
        }

        let cache = ECache::find()
            .filter(CCache::Name.eq(name.clone()))
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
            })?;

        if cache.is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(BaseResponse {
                    error: true,
                    message: "Cache Name already exists".to_string(),
                }),
            ));
        }

        acache.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: format!("Invalid display name: {}", e),
                }),
            ));
        }
        acache.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        acache.description = Set(description.trim().to_string());
    }

    if let Some(priority) = body.priority {
        acache.priority = Set(priority);
    }

    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to update cache: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
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

    // Prevent deletion of state-managed caches
    if cache.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot delete state-managed cache. This cache is managed by configuration and cannot be deleted through the API.".to_string(),
            }),
        ));
    }

    // Collect orgs that subscribe to this cache before deleting it, so we can
    // clean up orphaned NAR files in the background afterwards.
    let subscribing_orgs: Vec<Uuid> = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache.id))
        .all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|oc| oc.organization)
        .collect();

    let acache: ACache = cache.into();
    if let Err(e) = acache.delete(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to delete cache: {}", e),
            }),
        ));
    }

    // Spawn background task to delete now-orphaned NAR files.
    let state_bg = Arc::clone(&state);
    tokio::spawn(async move {
        cleanup_nars_for_orgs(state_bg, subscribing_orgs).await;
    });

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
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
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

    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to activate cache: {}", e),
            }),
        ));
    }

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
    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
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

    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    if let Err(e) = acache.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to deactivate cache: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if cache.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed cache.".to_string(),
        ));
    }

    let mut acache: ACache = cache.into();
    acache.public = Set(true);
    acache.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache is now public".to_string(),
    }))
}

pub async fn delete_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if cache.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed cache.".to_string(),
        ));
    }

    let mut acache: ACache = cache.into();
    acache.public = Set(false);
    acache.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache is now private".to_string(),
    }))
}
