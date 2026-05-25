/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD for cache-scoped custom roles.

use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{
    CachePermission, PermissionEntry, available_cache_permissions, cache_mask_to_vec,
    is_builtin_cache_role, parse_cache_permission_list,
};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::types::input::check_index_name;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Request / response shapes ─────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct CacheRoleResponse {
    pub id: RoleId,
    pub name: String,
    pub cache: Option<CacheId>,
    pub builtin: bool,
    pub managed: bool,
    pub permissions: Vec<&'static str>,
}

impl CacheRoleResponse {
    fn from_model(role: MCacheRole) -> Self {
        let builtin = is_builtin_cache_role(role.id);
        let permissions = cache_mask_to_vec(role.permission)
            .into_iter()
            .map(|p| p.as_wire_name())
            .collect();
        Self {
            id: role.id,
            name: role.name,
            cache: role.cache,
            builtin,
            managed: role.managed,
            permissions,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct CacheRoleListResponse {
    pub roles: Vec<CacheRoleResponse>,
    pub available_permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize, Debug)]
pub struct CreateCacheRoleRequest {
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchCacheRoleRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn load_cache_role(
    state: &Arc<ServerState>,
    cache_id: CacheId,
    role_id: RoleId,
) -> WebResult<MCacheRole> {
    let role = ECacheRole::find_by_id(role_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;
    if let Some(owner) = role.cache
        && owner != cache_id
    {
        return Err(WebError::not_found("Role"));
    }
    Ok(role)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /caches/{cache}/roles` - list roles available in the cache.
pub async fn get_cache_roles(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheRoleListResponse>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let roles = ECacheRole::find()
        .filter(
            Condition::any()
                .add(CCacheRole::Cache.is_null())
                .add(CCacheRole::Cache.eq(cache.id)),
        )
        .all(&state.web_db)
        .await?;

    Ok(ok_json(CacheRoleListResponse {
        roles: roles.into_iter().map(CacheRoleResponse::from_model).collect(),
        available_permissions: available_cache_permissions(),
    }))
}

/// `POST /caches/{cache}/roles` - create a custom role.
pub async fn post_cache_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<CreateCacheRoleRequest>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheRoles,
            reject_managed: true,
        },
    )
    .await?;

    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name("Role Name"));
    }

    let mask = parse_cache_permission_list(&body.permissions, "GET /caches/{cache}/roles")?;

    let clash = ECacheRole::find()
        .filter(CCacheRole::Name.eq(body.name.as_str()))
        .filter(
            Condition::any()
                .add(CCacheRole::Cache.eq(cache.id))
                .add(CCacheRole::Cache.is_null()),
        )
        .one(&state.web_db)
        .await?;
    if clash.is_some() {
        return Err(WebError::already_exists("Role Name"));
    }

    let role = ACacheRole {
        id: Set(RoleId::now_v7()),
        name: Set(body.name.clone()),
        cache: Set(Some(cache.id)),
        permission: Set(mask),
        managed: Set(false),
    }
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_ROLE_CREATE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "role_id": role.id.to_string(),
            "name": role.name,
            "permission_mask": mask,
        })),
    )
    .await;

    Ok(ok_json(CacheRoleResponse::from_model(role)))
}

/// `GET /caches/{cache}/roles/{role_id}` - fetch a single role.
pub async fn get_cache_role(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Member {
            reject_managed: false,
        },
    )
    .await?;
    let role = load_cache_role(&state, cache.id, role_id).await?;
    Ok(ok_json(CacheRoleResponse::from_model(role)))
}

/// `PATCH /caches/{cache}/roles/{role_id}` - update a custom role.
///
/// Built-in roles are immutable: attempting to mutate them returns 403.
pub async fn patch_cache_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, role_id)): Path<(String, RoleId)>,
    Json(body): Json<PatchCacheRoleRequest>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_cache_role(&state, cache.id, role_id).await?;

    if is_builtin_cache_role(role.id) {
        return Err(WebError::forbidden(
            "Built-in roles (Admin, Write, View) cannot be modified.",
        ));
    }

    if role.managed {
        return Err(WebError::forbidden(
            "State-managed roles cannot be modified via the API.",
        ));
    }

    let previous_mask = role.permission;
    let previous_name = role.name.clone();
    let mut active: ACacheRole = role.into_active_model();

    if let Some(name) = body.name {
        if check_index_name(&name).is_err() {
            return Err(WebError::invalid_name("Role Name"));
        }
        let clash = ECacheRole::find()
            .filter(CCacheRole::Name.eq(name.as_str()))
            .filter(CCacheRole::Id.ne(role_id))
            .filter(
                Condition::any()
                    .add(CCacheRole::Cache.eq(cache.id))
                    .add(CCacheRole::Cache.is_null()),
            )
            .one(&state.web_db)
            .await?;
        if clash.is_some() {
            return Err(WebError::already_exists("Role Name"));
        }
        active.name = Set(name);
    }

    if let Some(perms) = body.permissions {
        active.permission =
            Set(parse_cache_permission_list(&perms, "GET /caches/{cache}/roles")?);
    }

    let updated = active.update(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_ROLE_UPDATE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "role_id": updated.id.to_string(),
            "previous_name": previous_name,
            "previous_permission_mask": previous_mask,
            "new_name": updated.name,
            "new_permission_mask": updated.permission,
        })),
    )
    .await;

    Ok(ok_json(CacheRoleResponse::from_model(updated)))
}

/// `DELETE /caches/{cache}/roles/{role_id}` - delete a custom role.
///
/// Refuses to delete a role that is still in use; the caller must reassign
/// affected members first.
pub async fn delete_cache_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_cache_role(&state, cache.id, role_id).await?;

    if is_builtin_cache_role(role.id) {
        return Err(WebError::forbidden(
            "Built-in roles (Admin, Write, View) cannot be deleted.",
        ));
    }

    if role.managed {
        return Err(WebError::forbidden(
            "State-managed roles cannot be deleted via the API.",
        ));
    }

    let in_use = ECacheUser::find()
        .filter(CCacheUser::Role.eq(role_id))
        .filter(CCacheUser::Cache.eq(cache.id))
        .one(&state.web_db)
        .await?
        .is_some();
    if in_use {
        return Err(WebError::bad_request(
            "Role is still assigned to members. Reassign them before deleting the role.",
        ));
    }

    let role_name = role.name.clone();
    role.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_ROLE_DELETE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "role_id": role_id.to_string(),
            "name": role_name,
        })),
    )
    .await;

    Ok(ok_json(true))
}
