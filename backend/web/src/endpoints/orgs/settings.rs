/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::db::{get_any_cache_by_name, get_organization_by_name};
use core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_WRITE_ID};
use core::types::*;
use entity::organization_cache::CacheSubscriptionMode;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter,
};
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

pub async fn post_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization.".to_string(),
        ));
    }

    let mut aorganization: AOrganization = organization.into();
    aorganization.public = Set(true);
    aorganization.update(&state.db).await?;

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
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization.".to_string(),
        ));
    }

    let mut aorganization: AOrganization = organization.into();
    aorganization.public = Set(false);
    aorganization.update(&state.db).await?;

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
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization)
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let organization_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    let mut subscribed = Vec::new();
    for oc in organization_caches {
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
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await?;

    let has_write_permission = matches!(
        org_user,
        Some(ref ou) if ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID
    );

    if !has_write_permission {
        return Err(WebError::Forbidden(
            "You need Write or Admin permissions in this organization to manage cache subscriptions".to_string(),
        ));
    }

    let cache: MCache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && cache.created_by != user.id {
        return Err(WebError::Forbidden(
            "You don't have permission to subscribe to this cache. The cache is private and you are not the owner.".to_string(),
        ));
    }

    let already = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
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
        organization: Set(organization.id),
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
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await?;

    let has_write_permission = matches!(
        org_user,
        Some(ref ou) if ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID
    );

    if !has_write_permission {
        return Err(WebError::Forbidden(
            "You need Write or Admin permissions in this organization to manage cache subscriptions".to_string(),
        ));
    }

    let cache: MCache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let record = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
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
