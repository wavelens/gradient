/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::helpers::{OptionExt, ok_json};
use crate::error::{WebError, WebResult};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::db::get_cache_by_name;
use gradient_core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    pub id: CacheUpstreamId,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    /// Set for internal upstreams.
    pub upstream_cache_id: Option<CacheId>,
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

// ── Access helpers ────────────────────────────────────────────────────────────

async fn load_cache_for_user(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_name: String,
) -> WebResult<MCache> {
    get_cache_by_name(Arc::clone(state), user_id, cache_name)
        .await?
        .or_not_found("Cache")
}

async fn load_upstream(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    upstream_id: CacheUpstreamId,
) -> WebResult<MCacheUpstream> {
    ECacheUpstream::find_by_id(upstream_id)
        .filter(CCacheUpstream::Cache.eq(cache_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Upstream cache")
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn get_cache_upstreams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<UpstreamCacheItem>>>> {
    let cache = load_cache_for_user(&state, user.id, cache).await?;

    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.web_db)
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

    Ok(ok_json(upstreams))
}

pub async fn put_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<AddUpstreamRequest>,
) -> WebResult<Json<BaseResponse<CacheUpstreamId>>> {
    let cache = load_cache_for_user(&state, user.id, cache).await?;

    let record = match body {
        AddUpstreamRequest::Internal {
            cache_name,
            display_name,
            mode,
        } => {
            let upstream = load_cache_for_user(&state, user.id, cache_name).await?;
            if upstream.id == cache.id {
                return Err(WebError::bad_request(
                    "A cache cannot be its own upstream".to_string(),
                ));
            }
            let name = display_name.unwrap_or_else(|| upstream.display_name.clone());
            ACacheUpstream {
                id: Set(CacheUpstreamId::now_v7()),
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
            id: Set(CacheUpstreamId::now_v7()),
            cache: Set(cache.id),
            display_name: Set(display_name),
            mode: Set(CacheSubscriptionMode::ReadOnly),
            upstream_cache: Set(None),
            url: Set(Some(url)),
            public_key: Set(Some(public_key)),
        },
    };

    let inserted = record.insert(&state.web_db).await?;
    Ok(ok_json(inserted.id))
}

pub async fn patch_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, CacheUpstreamId)>,
    Json(body): Json<PatchUpstreamRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache_for_user(&state, user.id, cache).await?;
    let record = load_upstream(&state, cache.id, upstream_id).await?;

    let is_external = matches!(
        record.as_source(),
        Some(entity::cache_upstream::CacheUpstreamSource::External { .. })
    );
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

    active.update(&state.web_db).await?;

    Ok(ok_json("Upstream updated".to_string()))
}

pub async fn delete_cache_upstream(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((cache, upstream_id)): Path<(String, CacheUpstreamId)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache_for_user(&state, user.id, cache).await?;
    let record = load_upstream(&state, cache.id, upstream_id).await?;

    let active: ACacheUpstream = record.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json("Upstream removed".to_string()))
}
