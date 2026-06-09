/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::cleanup_nars_for_orgs;
use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::CachePermission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDateTime;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::organization_cache::CacheSubscriptionMode;
use gradient_core::sources::{format_cache_public_key, generate_signing_key};
use gradient_core::types::input::{check_index_name, validate_display_name};
use gradient_core::types::*;
use gradient_core::ServerState;
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
    #[serde(default)]
    pub local_priority: Option<i32>,
    #[serde(default)]
    pub max_storage_gb: Option<i32>,
}

#[derive(Serialize)]
pub struct CacheResponse {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub max_storage_gb: i32,
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
    pub local_priority: Option<i32>,
    pub max_storage_gb: Option<i32>,
}

fn validate_max_storage_gb(value: i32) -> WebResult<()> {
    if value < 0 {
        return Err(WebError::bad_request(
            "max_storage_gb must be 0 (unlimited) or at least 1",
        ));
    }
    Ok(())
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

    let max_storage_gb = body.max_storage_gb.unwrap_or(0);
    validate_max_storage_gb(max_storage_gb)?;

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
        local_priority: Set(body.local_priority),
        public_key: Set(public_key),
        private_key: Set(private_key),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
        max_storage_gb: Set(max_storage_gb),
    }
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, "Cache Name"))?;

    ACacheUpstream {
        id: Set(CacheUpstreamId::now_v7()),
        cache: Set(cache.id),
        display_name: Set("cache.nixos.org".to_string()),
        mode: Set(CacheSubscriptionMode::ReadOnly),
        kind: Set(CacheUpstreamKind::Http),
        upstream_cache: Set(None),
        url: Set(Some("https://cache.nixos.org".to_string())),
        public_key: Set(Some(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".to_string(),
        )),
        remote_cache_name: Set(None),
        api_key: Set(None),
    }
    .insert(&tx)
    .await?;

    ACacheUser {
        id: Set(CacheUserId::now_v7()),
        cache: Set(cache.id),
        user: Set(user.id),
        role: Set(BASE_CACHE_ROLE_ADMIN_ID),
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheResponse>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_secret_file,
        cache.clone(),
        state.config.server.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to derive public key");
        WebError::internal("Failed to derive public key")
    })?;

    let can_edit = match &maybe_user {
        Some(u) => {
            let mem = ECacheUser::find()
                .filter(CCacheUser::Cache.eq(cache.id))
                .filter(CCacheUser::User.eq(u.id))
                .one(&state.web_db)
                .await
                .unwrap_or(None);
            if let Some(m) = mem {
                let role = ECacheRole::find_by_id(m.role)
                    .one(&state.web_db)
                    .await
                    .unwrap_or(None);
                role.map(|r| {
                    crate::permissions::cache_mask_grants(
                        r.permission,
                        CachePermission::ManageCacheSettings,
                    )
                })
                .unwrap_or(false)
            } else {
                false
            }
        }
        None => false,
    };

    Ok(ok_json(CacheResponse {
        id: cache.id,
        name: cache.name,
        display_name: cache.display_name,
        description: cache.description,
        active: cache.active,
        priority: cache.priority,
        local_priority: cache.local_priority,
        max_storage_gb: cache.max_storage_gb,
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<PatchCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        },
    )
    .await?;
    let cache_id = cache.id;
    let prev_max_storage_gb = cache.max_storage_gb;
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

    if let Some(local_priority) = body.local_priority {
        acache.local_priority = Set(Some(local_priority));
    }

    let mut raised_limit = false;
    if let Some(max_storage_gb) = body.max_storage_gb {
        validate_max_storage_gb(max_storage_gb)?;
        raised_limit = max_storage_gb == 0 || max_storage_gb > prev_max_storage_gb;
        acache.max_storage_gb = Set(max_storage_gb);
    }

    acache
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Cache Name"))?;

    if raised_limit {
        let org_ids: Vec<OrganizationId> = EOrganizationCache::find()
            .filter(COrganizationCache::Cache.eq(cache_id))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|oc| oc.organization)
            .collect();
        for org in org_ids {
            if let Err(e) = gradient_core::ci::unpark_storage_full_for_org(
                &state.web_db,
                org,
                state.config.storage.max_storage_gb,
            )
            .await
            {
                tracing::warn!(error = %e, org_id = %org, "failed to unpark storage-full evals");
            }
        }
    }

    Ok(ok_json("Cache updated".to_string()))
}

pub async fn delete_cache(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::DeleteCache,
            reject_managed: true,
        },
    )
    .await?;
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut acache: ACache = cache.into();
    acache.active = Set(true);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache enabled".to_string()))
}

pub async fn delete_cache_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut acache: ACache = cache.into();
    acache.active = Set(false);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache disabled".to_string()))
}

pub async fn post_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut acache: ACache = cache.into();
    acache.public = Set(true);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache is now public".to_string()))
}

pub async fn delete_cache_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut acache: ACache = cache.into();
    acache.public = Set(false);
    acache.update(&state.web_db).await?;

    Ok(ok_json("Cache is now private".to_string()))
}

#[cfg(test)]
mod tests {
    use super::validate_max_storage_gb;

    #[test]
    fn validate_max_storage_gb_accepts_zero_and_positive() {
        assert!(validate_max_storage_gb(0).is_ok());
        assert!(validate_max_storage_gb(1).is_ok());
        assert!(validate_max_storage_gb(500).is_ok());
    }

    #[test]
    fn validate_max_storage_gb_rejects_negative() {
        assert!(validate_max_storage_gb(-1).is_err());
    }
}
