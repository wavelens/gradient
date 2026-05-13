/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::cleanup_nars_for_orgs;
use crate::access::{CacheAccess, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::Extension;
use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use std::net::SocketAddr;
use chrono::NaiveDateTime;
use entity::organization_cache::CacheSubscriptionMode;
use gradient_core::db::get_any_cache_by_name;
use gradient_core::sources::{format_cache_public_key, generate_signing_key};
use gradient_core::types::input::{check_index_name, validate_display_name};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub public_key: String,
    pub public: bool,
    pub created_by: UserId,
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

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn get_cache_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let exists = ECache::find()
        .filter(CCache::Name.eq(name.as_str()))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    // TODO: Implement pagination
    // Find all orgs the user belongs to
    let org_memberships = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.web_db)
        .await?;

    let org_ids: Vec<OrganizationId> = org_memberships
        .into_iter()
        .map(|m| m.organization)
        .collect();

    // Find cache IDs subscribed by those orgs
    let org_cache_ids: Vec<CacheId> = if org_ids.is_empty() {
        vec![]
    } else {
        EOrganizationCache::find()
            .filter(COrganizationCache::Organization.is_in(org_ids))
            .all(&state.web_db)
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
        .all(&state.web_db)
        .await?;

    Ok(ok_json(caches))
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
        return Err(WebError::bad_request(format!(
            "Invalid display name: {}",
            e
        )));
    }

    let existing_cache = ECache::find()
        .filter(CCache::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;

    if existing_cache.is_some() {
        return Err(WebError::already_exists("Cache Name"));
    }

    let (private_key, public_key) = generate_signing_key(&state.config.secrets.crypt_secret_file)
        .map_err(|e| {
        tracing::error!(error = %e, "Failed to generate signing key");
        WebError::internal("Failed to generate signing key")
    })?;

    let tx = state.web_db.inner().begin().await?;

    let cache = ACache {
        id: Set(CacheId::now_v7()),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        priority: Set(body.priority),
        local_priority: Set(None),
        public_key: Set(public_key),
        private_key: Set(private_key),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
    }
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, "Cache Name"))?;

    ACacheUpstream {
        id: Set(CacheUpstreamId::now_v7()),
        cache: Set(cache.id),
        display_name: Set("cache.nixos.org".to_string()),
        mode: Set(CacheSubscriptionMode::ReadOnly),
        upstream_cache: Set(None),
        url: Set(Some("https://cache.nixos.org".to_string())),
        public_key: Set(Some(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".to_string(),
        )),
    }
    .insert(&tx)
    .await?;

    tx.commit().await?;

    Ok(ok_json(cache.id.to_string()))
}

pub async fn get_public_caches(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    let caches = ECache::find()
        .filter(CCache::Public.eq(true))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(caches))
}

pub async fn get_cache(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheResponse>>> {
    let cache: MCache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .or_not_found("Cache")?;

    if !cache.public {
        match &maybe_user {
            Some(user) if cache.created_by == user.id => {}
            _ => return Err(WebError::not_found("Cache")),
        }
    }

    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_secret_file,
        cache.clone(),
        state.config.server.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to derive public key");
        WebError::internal("Failed to derive public key")
    })?;

    let can_edit = matches!(&maybe_user, Some(u) if u.id == cache.created_by);

    Ok(ok_json(CacheResponse {
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
    }))
}

pub async fn patch_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
    Json(body): Json<PatchCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let mut acache: ACache = cache.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Cache Name"));
        }
        if ECache::find()
            .filter(CCache::Name.eq(name.clone()))
            .one(&state.web_db)
            .await?
            .is_some()
        {
            return Err(WebError::already_exists("Cache Name"));
        }
        acache.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::bad_request(format!(
                "Invalid display name: {}",
                e
            )));
        }
        acache.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        acache.description = Set(description.trim().to_string());
    }

    if let Some(priority) = body.priority {
        acache.priority = Set(priority);
    }

    acache
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Cache Name"))?;

    Ok(ok_json("Cache updated".to_string()))
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let cache_id = cache.id;
    let cache_name = cache.name.clone();

    let subscribing_orgs: Vec<OrganizationId> = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache.id))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|oc| oc.organization)
        .collect();

    let acache: ACache = cache.into();
    acache.delete(&state.web_db).await?;

    let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_DELETE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache_id.to_string(),
            "cache_name": cache_name,
            "subscribing_orgs": subscribing_orgs.iter().map(|o| o.to_string()).collect::<Vec<_>>(),
        })),
    )
    .await;

    let state_bg = Arc::clone(&state);
    state.shutdown.spawn(async move {
        cleanup_nars_for_orgs(state_bg, subscribing_orgs).await;
    });

    Ok(ok_json("Cache deleted".to_string()))
}

pub async fn post_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache enabled".to_string()))
}

pub async fn delete_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache disabled".to_string()))
}

pub async fn post_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let mut acache: ACache = cache.into();
    acache.public = Set(true);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache is now public".to_string()))
}

pub async fn delete_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(&state, user.id, cache, CacheAccess::Editable).await?;
    let mut acache: ACache = cache.into();
    acache.public = Set(false);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache is now private".to_string()))
}
