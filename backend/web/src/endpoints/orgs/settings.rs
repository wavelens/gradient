/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{load_editable_org, load_org_member};
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::db::get_any_cache_by_name;
use core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_WRITE_ID};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct SubscribeCacheRequest {
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Serialize)]
pub struct CacheSubscriptionItem {
    pub id: Uuid,
    pub name: String,
    pub mode: CacheSubscriptionMode,
}

// ── Access helpers ────────────────────────────────────────────────────────────

/// Verify that `user_id` has Write or Admin role in `org_id`.
async fn require_write_permission(
    state: &Arc<ServerState>,
    org_id: Uuid,
    user_id: Uuid,
) -> WebResult<()> {
    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(org_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?;

    let has_write = matches!(
        org_user,
        Some(ref ou) if ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID
    );

    if !has_write {
        return Err(WebError::Forbidden(
            "You need Write or Admin permissions in this organization to manage cache subscriptions"
                .to_string(),
        ));
    }

    Ok(())
}

/// Load a public or owned cache by name; verify the requesting user may subscribe to it.
async fn load_subscribable_cache(
    state: &Arc<ServerState>,
    cache_name: String,
    user_id: Uuid,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && cache.created_by != user_id {
        return Err(WebError::Forbidden(
            "You don't have permission to subscribe to this cache. The cache is private and you are not the owner.".to_string(),
        ));
    }

    Ok(cache)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn post_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let mut active: AOrganization = org.into();
    active.public = Set(true);
    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now public".to_string(),
    }))
}

pub async fn delete_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_editable_org(&state, user.id, organization).await?;
    let mut active: AOrganization = org.into();
    active.public = Set(false);
    active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now private".to_string(),
    }))
}

pub async fn get_organization_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<CacheSubscriptionItem>>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org.id))
        .all(&state.db)
        .await?;

    let mut subscribed = Vec::new();
    for oc in org_caches {
        if let Ok(Some(cache)) = ECache::find_by_id(oc.cache).one(&state.db).await {
            subscribed.push(CacheSubscriptionItem {
                id: oc.cache,
                name: cache.name,
                mode: oc.mode,
            });
        }
    }

    Ok(Json(BaseResponse {
        error: false,
        message: subscribed,
    }))
}

pub async fn post_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
    body: Option<Json<SubscribeCacheRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;
    require_write_permission(&state, org.id, user.id).await?;

    let cache = load_subscribable_cache(&state, cache, user.id).await?;

    let already = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?;

    if already.is_some() {
        return Err(WebError::already_exists(
            "Organization already subscribed to Cache",
        ));
    }

    let mode = body
        .and_then(|b| b.mode.clone())
        .unwrap_or(CacheSubscriptionMode::ReadWrite);

    AOrganizationCache {
        id: Set(Uuid::new_v4()),
        organization: Set(org.id),
        cache: Set(cache.id),
        mode: Set(mode),
    }
    .insert(&state.db)
    .await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache subscribed".to_string(),
    }))
}

pub async fn delete_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;
    require_write_permission(&state, org.id, user.id).await?;

    let cache = load_subscribable_cache(&state, cache, user.id).await?;

    let record = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(org.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("Organization not subscribed to Cache".to_string()))?;

    let active: AOrganizationCache = record.into();
    active.delete(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Cache unsubscribed".to_string(),
    }))
}
