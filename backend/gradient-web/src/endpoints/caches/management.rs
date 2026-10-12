/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::CacheRefusal;
use crate::api_spec::CachePath;
use crate::audit::{Audit, changed_fields};
use crate::error::{WebError, WebResult, require_create_permission};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::names::{CacheDisplayName, CacheName};
use crate::permissions::CachePermission;
use crate::resource::Resource;
use crate::scope::{CacheScope, Readable, Requires, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::extract::{Query, State};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_sources::{format_cache_public_key, generate_signing_key};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeCacheRequest {
    pub name: CacheName,
    pub display_name: CacheDisplayName,
    pub description: String,
    pub priority: i32,
    pub public: Option<bool>,
    #[serde(default)]
    pub local_priority: Option<i32>,
    #[serde(default)]
    pub max_storage_gb: Option<i32>,
    #[serde(default)]
    pub pull_through: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CacheResponse {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub max_storage_gb: i32,
    pub pull_through: bool,
    pub public_key: String,
    pub public: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub can_edit: bool,
}

impl CacheResponse {
    fn new(cache: MCache, public_key: String, can_edit: bool) -> Self {
        Self {
            id: cache.id,
            name: cache.name,
            display_name: cache.display_name,
            description: cache.description,
            active: cache.active,
            priority: cache.priority,
            local_priority: cache.local_priority,
            max_storage_gb: cache.max_storage_gb,
            pull_through: cache.pull_through,
            public_key,
            public: cache.public,
            created_by: cache.created_by,
            created_at: cache.created_at,
            managed: cache.managed,
            can_edit,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchCacheRequest {
    pub name: Option<CacheName>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub local_priority: Option<i32>,
    pub max_storage_gb: Option<i32>,
    pub pull_through: Option<bool>,
}

fn validate_max_storage_gb(value: i32) -> WebResult<()> {
    if value < 0 {
        return Err(CacheRefusal::NegativeMaxStorage.into());
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/caches/available",
    operation_id = "checkCacheNameAvailable",
    tag = "caches",
    summary = "Check cache name availability",
    description = "Returns `true` if no cache with the given name exists, `false` if it is already taken.",
    params(("name" = String, Query)),
    responses((status = OK, description = "Availability result", body = bool, content_type = "application/json"), WebError)
)]
pub async fn get_cache_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    let Ok(name) = CacheName::try_from(name) else {
        return Ok(ok_json(false));
    };
    let exists = gradient_db::caches::by_name(&state.web_db, name.as_str())
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

type PaginatedCache = Paginated<MCache>;

#[utoipa::path(
    get,
    path = "/caches",
    operation_id = "listCaches",
    tag = "caches",
    summary = "List caches",
    description = "Returns a paginated list of binary caches the current user owns or can access through a project subscription.",
    params(PaginationParams),
    responses((status = OK, description = "Paginated cache list", body = PaginatedCache), WebError)
)]
pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<MCache>>>> {
    let condition = crate::access::visible_cache_condition(&state.web_db, user.id).await?;
    let listing = crate::helpers::paginate(
        gradient_db::caches::ordered_by_name(condition),
        &state.web_db,
        &params,
    )
    .await?;

    Ok(ok_json(listing))
}

#[utoipa::path(
    put,
    path = "/caches",
    operation_id = "createCache",
    tag = "caches",
    summary = "Create cache",
    description = "Creates a new binary cache with an auto-generated signing key.",
    request_body = MakeCacheRequest,
    responses((status = OK, description = "Cache UUID", body = String, content_type = "application/json"), WebError)
)]
pub async fn put(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<MakeCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_create_permission(state.config.permissions.create_cache, &user)?;

    let max_storage_gb = body.max_storage_gb.unwrap_or(0);
    validate_max_storage_gb(max_storage_gb)?;

    let existing_cache = gradient_db::caches::by_name(&state.web_db, body.name.as_str()).await?;

    if existing_cache.is_some() {
        return Err(WebError::already_exists(Resource::CacheName));
    }

    let (private_key, public_key) =
        generate_signing_key(&state.config.secrets.crypt_file).context("generate signing key")?;

    let tx = state.web_db.inner().begin().await?;

    let cache = gradient_db::rows::insert::<ECache, _>(
        &tx,
        MCache {
            id: CacheId::now_v7(),
            name: body.name.to_string(),
            active: true,
            display_name: body.display_name.trim().to_string(),
            description: body.description.trim().to_string(),
            priority: body.priority,
            local_priority: body.local_priority,
            public_key,
            private_key,
            public: body.public.unwrap_or(false),
            created_by: user.id,
            created_at: gradient_types::now(),
            max_storage_gb,
            pull_through: body.pull_through.unwrap_or(true),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::CacheName))?;

    gradient_db::rows::insert::<ECacheUpstream, _>(
        &tx,
        MCacheUpstream {
            id: CacheUpstreamId::now_v7(),
            cache: cache.id,
            display_name: NIXOS_CACHE_NAME.to_string(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind: CacheUpstreamKind::Http,
            url: Some(NIXOS_CACHE_URL.to_string()),
            public_key: Some(NIXOS_CACHE_PUBLIC_KEY.to_string()),
            ..Default::default()
        },
    )
    .await?;

    gradient_db::rows::insert::<ECacheUser, _>(
        &tx,
        MCacheUser {
            id: CacheUserId::now_v7(),
            cache: cache.id,
            user: user.id,
            role: BASE_CACHE_ROLE_ADMIN_ID,
        },
    )
    .await?;

    tx.commit().await?;

    audit
        .write(
            Action::CacheCreate,
            EventOwner::cache(cache.id),
            serde_json::json!({ "cache_id": cache.id.to_string(), "name": cache.name, }),
        )
        .await;

    Ok(ok_json(cache.id.to_string()))
}

#[utoipa::path(
    get,
    path = "/caches/public",
    operation_id = "listPublicCaches",
    tag = "caches",
    summary = "List public caches",
    description = "Returns every public cache. Does not require authentication.",
    responses((status = OK, description = "Public cache list", body = Vec<MCache>), WebError)
)]
pub async fn get_public_caches(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<Vec<MCache>>>> {
    let caches = gradient_db::caches::all_public(&state.web_db).await?;

    Ok(ok_json(caches))
}

#[utoipa::path(
    get,
    path = "/caches/{cache}",
    operation_id = "getCache",
    tag = "caches",
    summary = "Get cache",
    description = "Returns cache details. Public caches are visible to anyone. A private cache is visible to its cache members and to members of any project subscribed to it (matching the visibility of `GET /caches`); other callers receive `404`.",
    params(CachePath),
    responses((status = OK, description = "Cache details", body = CacheResponse), WebError)
)]
pub async fn get_cache(
    CacheScope {
        cache,
        user: maybe_user,
        ..
    }: CacheScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<CacheResponse>>> {
    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_file,
        cache.clone(),
        state.config.server.serve_url.clone(),
    )
    .context("derive public key")?;

    let can_edit = match &maybe_user {
        Some(u) => gradient_db::access::cache_permission_mask(&state.web_db, cache.id, u.id)
            .await
            .ok()
            .flatten()
            .is_some_and(|mask| {
                crate::permissions::cache_mask_grants(mask, CachePermission::ManageCacheSettings)
            }),
        None => false,
    };

    Ok(ok_json(CacheResponse::new(cache, public_key, can_edit)))
}

#[utoipa::path(
    patch,
    path = "/caches/{cache}",
    operation_id = "patchCache",
    tag = "caches",
    summary = "Update cache",
    description = "Updates one or more cache fields. Requires `manageCacheSettings`. Managed caches cannot be edited via the API.",
    params(CachePath),
    request_body = PatchCacheRequest,
    responses((status = OK, description = "Updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_cache(
    CacheScope { cache, .. }: CacheScope<RequiresUnmanaged<can::ManageCacheSettings>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<PatchCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache_id = cache.id;
    let prev_max_storage_gb = cache.max_storage_gb;
    let fields = changed_fields([
        ("name", body.name.is_some()),
        ("display_name", body.display_name.is_some()),
        ("description", body.description.is_some()),
        ("priority", body.priority.is_some()),
        ("local_priority", body.local_priority.is_some()),
        ("max_storage_gb", body.max_storage_gb.is_some()),
        ("pull_through", body.pull_through.is_some()),
    ]);
    let mut acache: ACache = cache.into();

    if let Some(name) = body.name {
        if gradient_db::caches::by_name(&state.web_db, name.as_str())
            .await?
            .is_some()
        {
            return Err(WebError::already_exists(Resource::CacheName));
        }
        acache.name = Set(name.into_inner());
    }

    if let Some(display_name) = body.display_name {
        let display_name = CacheDisplayName::try_from(display_name.trim().to_string())?;
        acache.display_name = Set(display_name.into_inner());
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

    if let Some(pull_through) = body.pull_through {
        acache.pull_through = Set(pull_through);
    }

    let mut raised_limit = false;
    if let Some(max_storage_gb) = body.max_storage_gb {
        validate_max_storage_gb(max_storage_gb)?;
        raised_limit = max_storage_gb == 0 || max_storage_gb > prev_max_storage_gb;
        acache.max_storage_gb = Set(max_storage_gb);
    }

    gradient_db::rows::update::<ECache, _>(&state.web_db, acache)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::CacheName))?;

    if raised_limit {
        let project_ids: Vec<ProjectId> =
            gradient_db::projects::caches::subscriptions_to(&state.web_db, cache_id)
                .await?
                .into_iter()
                .map(|oc| oc.project)
                .collect();
        for project in project_ids {
            if let Err(e) = gradient_ci::unpark_storage_full_for_project(
                &state.web_db,
                project,
                state.config.cache.max_storage_gb,
            )
            .await
            {
                tracing::warn!(error = %e, project_id = %project, "failed to unpark storage-full evals");
            }
        }
    }

    audit
        .write(Action::CacheUpdate, EventOwner::cache(cache_id), fields)
        .await;

    Ok(ok_json("Cache updated".to_string()))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}",
    operation_id = "deleteCache",
    tag = "caches",
    summary = "Delete cache",
    description = "Deletes the cache. Requires `deleteCache`. Managed caches cannot be deleted via the API.",
    params(CachePath),
    responses((status = OK, description = "Deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_cache(
    CacheScope { cache, db, .. }: CacheScope<RequiresUnmanaged<can::DeleteCache>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache_id = cache.id;
    let cache_name = cache.name.clone();

    let subscribing_projects: Vec<ProjectId> =
        gradient_db::projects::caches::subscriptions_to(&db.web_db, cache.id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|oc| oc.project)
            .collect();

    gradient_db::rows::delete::<ECache, _>(&db.web_db, cache).await?;

    audit.write(Action::CacheDelete, EventOwner::cache(cache_id), serde_json::json!({ "cache_id": cache_id.to_string(), "cache_name": cache_name, "subscribing_projects": subscribing_projects.iter().map(|o| o.to_string()).collect::<Vec<_>>(), })).await;

    Ok(ok_json("Cache deleted".to_string()))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/active",
    operation_id = "enableCache",
    tag = "caches",
    summary = "Enable cache",
    description = "Also allowed on a state-managed cache. The next server start restores the declared `active` value.",
    params(CachePath),
    responses((status = OK, description = "Enabled", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_cache_active(
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ManageCacheSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::caches::set_active(&db.web_db, cache, true).await?;

    Ok(ok_json("Cache enabled".to_string()))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}/active",
    operation_id = "disableCache",
    tag = "caches",
    summary = "Disable cache",
    description = "Also allowed on a state-managed cache. The next server start restores the declared `active` value.",
    params(CachePath),
    responses((status = OK, description = "Disabled", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_cache_active(
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ManageCacheSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::caches::set_active(&db.web_db, cache, false).await?;

    Ok(ok_json("Cache disabled".to_string()))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/public",
    operation_id = "makeCachePublic",
    tag = "caches",
    summary = "Make cache public",
    description = "Makes the cache readable without authentication, by the API and by Nix clients.",
    params(CachePath),
    responses((status = OK, description = "Cache is now public", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_cache_public(
    CacheScope { cache, db, .. }: CacheScope<RequiresUnmanaged<can::ManageCacheSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::caches::set_public(&db.web_db, cache, true).await?;

    Ok(ok_json("Cache is now public".to_string()))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}/public",
    operation_id = "makeCachePrivate",
    tag = "caches",
    summary = "Make cache private",
    description = "Requires credentials to read the cache, by the API and by Nix clients.",
    params(CachePath),
    responses((status = OK, description = "Cache is now private", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_cache_public(
    CacheScope { cache, db, .. }: CacheScope<RequiresUnmanaged<can::ManageCacheSettings>>,
) -> WebResult<Json<BaseResponse<String>>> {
    gradient_db::caches::set_public(&db.web_db, cache, false).await?;

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
