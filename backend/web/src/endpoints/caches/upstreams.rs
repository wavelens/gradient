/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::Extension;
use core::db::get_cache_by_name;
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddUpstreamRequest {
    /// An upstream that is another Gradient-managed cache (referenced by name).
    Internal {
        cache_name: String,
        display_name: Option<String>,
        mode: Option<CacheSubscriptionMode>,
    },
    /// An upstream that is an external Nix binary cache. Always ReadOnly.
    External {
        display_name: String,
        url: String,
        public_key: String,
    },
}

#[derive(Serialize)]
pub struct UpstreamCacheItem {
    pub id: Uuid,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    /// Set for internal upstreams.
    pub upstream_cache_id: Option<Uuid>,
    /// Set for external upstreams.
    pub url: Option<String>,
    pub public_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchUpstreamRequest {
    pub display_name: Option<String>,
    pub mode: Option<CacheSubscriptionMode>,
    pub url: Option<String>,
    pub public_key: Option<String>,
}

pub async fn get_cache_upstreams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|u| UpstreamCacheItem {
            id: u.id,
            display_name: u.display_name,
            mode: u.mode,
            upstream_cache_id: u.upstream_cache,
            url: u.url,
            public_key: u.public_key,
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: upstreams,
    }))
}

pub async fn put_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<AddUpstreamRequest>,
) -> WebResult<Json<BaseResponse<Uuid>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = match body {
        AddUpstreamRequest::Internal {
            cache_name,
            display_name,
            mode,
        } => {
            let upstream = get_cache_by_name(state.0.clone(), user.id, cache_name.clone())
                .await?
                .ok_or_else(|| WebError::not_found("Upstream cache"))?;
            if upstream.id == cache.id {
                return Err(WebError::BadRequest(
                    "A cache cannot be its own upstream".to_string(),
                ));
            }
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            ACacheUpstream {
                id: Set(Uuid::new_v4()),
                cache: Set(cache.id),
                display_name: Set(name),
                mode: Set(mode.unwrap_or(CacheSubscriptionMode::ReadWrite)),
                upstream_cache: Set(Some(upstream.id)),
                url: Set(None),
                public_key: Set(None),
            }
        }
        AddUpstreamRequest::External {
            display_name,
            url,
            public_key,
        } => ACacheUpstream {
            id: Set(Uuid::new_v4()),
            cache: Set(cache.id),
            display_name: Set(display_name),
            mode: Set(CacheSubscriptionMode::ReadOnly),
            upstream_cache: Set(None),
            url: Set(Some(url)),
            public_key: Set(Some(public_key)),
        },
    };

    let inserted = record.insert(&state.db).await?;
    Ok(Json(BaseResponse {
        error: false,
        message: inserted.id,
    }))
}

pub async fn patch_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, Uuid)>,
    Json(body): Json<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Upstream cache"))?;

    let is_external = record.upstream_cache.is_none();
    let mut active = record.into_active_model();

    if let Some(name) = body.display_name {
        active.display_name = Set(name);
    }
    if is_external {
        // External upstreams are always ReadOnly
        active.mode = Set(CacheSubscriptionMode::ReadOnly);
        if let Some(url) = body.url {
            active.url = Set(Some(url));
        }
        if let Some(key) = body.public_key {
            active.public_key = Set(Some(key));
        }
    } else if let Some(mode) = body.mode {
        active.mode = Set(mode);
    }

    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Upstream updated".to_string(),
    }))
}

pub async fn delete_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_cache_by_name(state.0.clone(), user.id, cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Upstream cache"))?;

    let active: ACacheUpstream = record.into();
    active.delete(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Upstream removed".to_string(),
    }))
}
