/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::lookup::require;
use crate::permissions::{
    PermissionEntry, available_cache_permissions, cache_mask_to_vec, is_builtin_cache_role,
    parse_cache_permission_list,
};
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use crate::scope::{CacheScope, Member, RequiresUnmanaged, can};
use axum::Json;
use gradient_db::WebDb;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_index_name;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug)]
pub struct CacheRoleResponse {
    pub id: RoleId,
    pub name: String,
    pub cache: Option<CacheId>,
    pub builtin: bool,
    pub managed: bool,
    pub permissions: Vec<&'static str>,
}

impl From<MCacheRole> for CacheRoleResponse {
    fn from(role: MCacheRole) -> Self {
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

async fn load_cache_role(db: &WebDb, cache_id: CacheId, role_id: RoleId) -> WebResult<MCacheRole> {
    let role = require::<ECacheRole>(db, role_id).await?;
    if let Some(owner) = role.cache
        && owner != cache_id
    {
        return Err(WebError::not_found(Resource::Role));
    }
    Ok(role)
}

pub async fn get_cache_roles(
    CacheScope { cache, db, .. }: CacheScope<Member>,
) -> WebResult<Json<BaseResponse<CacheRoleListResponse>>> {
    let roles = ECacheRole::find()
        .filter(
            Condition::any()
                .add(CCacheRole::Cache.is_null())
                .add(CCacheRole::Cache.eq(cache.id)),
        )
        .all(&db.web_db)
        .await?;

    Ok(ok_json(CacheRoleListResponse {
        roles: roles.into_iter().map(CacheRoleResponse::from).collect(),
        available_permissions: available_cache_permissions(),
    }))
}

pub async fn post_cache_role(
    CacheScope {
        cache, user, db, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheRoles>>,
    info: RequestInfo,
    Json(body): Json<CreateCacheRoleRequest>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name(Resource::RoleName));
    }

    let mask = parse_cache_permission_list(&body.permissions, "GET /caches/{cache}/roles")?;

    let clash = ECacheRole::find()
        .filter(CCacheRole::Name.eq(body.name.as_str()))
        .filter(
            Condition::any()
                .add(CCacheRole::Cache.eq(cache.id))
                .add(CCacheRole::Cache.is_null()),
        )
        .one(&db.web_db)
        .await?;
    if clash.is_some() {
        return Err(WebError::already_exists(Resource::RoleName));
    }

    let role = MCacheRole {
        id: RoleId::now_v7(),
        name: body.name.clone(),
        cache: Some(cache.id),
        permission: mask,
        ..Default::default()
    }
    .into_active_model()
    .insert(&db.web_db)
    .await?;

    audit_record(
        &db,
        Some(user.id),
        Action::CacheRoleCreate,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "role_id": role.id.to_string(),
            "name": role.name,
            "permission_mask": mask,
        })),
    )
    .await;

    Ok(ok_json(CacheRoleResponse::from(role)))
}

pub async fn get_cache_role(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<Member>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    let role_id: RoleId = params.param("role_id")?;

    let role = load_cache_role(&db.web_db, cache.id, role_id).await?;
    Ok(ok_json(CacheRoleResponse::from(role)))
}

pub async fn patch_cache_role(
    CacheScope {
        cache,
        user,
        db,
        params,
        ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheRoles>>,
    info: RequestInfo,
    Json(body): Json<PatchCacheRoleRequest>,
) -> WebResult<Json<BaseResponse<CacheRoleResponse>>> {
    let role_id: RoleId = params.param("role_id")?;

    let role = load_cache_role(&db.web_db, cache.id, role_id).await?;

    if is_builtin_cache_role(role.id) {
        return Err(SharedRefusal::BuiltinRole(Change::Modify).into());
    }

    if role.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Role).into());
    }

    let previous_mask = role.permission;
    let previous_name = role.name.clone();
    let mut active: ACacheRole = role.into_active_model();

    if let Some(name) = body.name {
        if check_index_name(&name).is_err() {
            return Err(WebError::invalid_name(Resource::RoleName));
        }
        let clash = ECacheRole::find()
            .filter(CCacheRole::Name.eq(name.as_str()))
            .filter(CCacheRole::Id.ne(role_id))
            .filter(
                Condition::any()
                    .add(CCacheRole::Cache.eq(cache.id))
                    .add(CCacheRole::Cache.is_null()),
            )
            .one(&db.web_db)
            .await?;
        if clash.is_some() {
            return Err(WebError::already_exists(Resource::RoleName));
        }
        active.name = Set(name);
    }

    if let Some(perms) = body.permissions {
        active.permission = Set(parse_cache_permission_list(
            &perms,
            "GET /caches/{cache}/roles",
        )?);
    }

    let updated = active.update(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::CacheRoleUpdate,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
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

    Ok(ok_json(CacheRoleResponse::from(updated)))
}

pub async fn delete_cache_role(
    CacheScope {
        cache,
        user,
        db,
        params,
        ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheRoles>>,
    info: RequestInfo,
) -> WebResult<Json<BaseResponse<bool>>> {
    let role_id: RoleId = params.param("role_id")?;

    let role = load_cache_role(&db.web_db, cache.id, role_id).await?;

    if is_builtin_cache_role(role.id) {
        return Err(SharedRefusal::BuiltinRole(Change::Delete).into());
    }

    if role.managed {
        return Err(SharedRefusal::StateManaged(Change::Delete, Resource::Role).into());
    }

    let in_use = ECacheUser::find()
        .filter(CCacheUser::Role.eq(role_id))
        .filter(CCacheUser::Cache.eq(cache.id))
        .one(&db.web_db)
        .await?
        .is_some();
    if in_use {
        return Err(SharedRefusal::RoleAssignedToMembers.into());
    }
    if gradient_db::teams::grants::cache_role_granted_to_a_team(&db.web_db, role_id).await? {
        return Err(SharedRefusal::RoleGrantedToTeam.into());
    }

    let role_name = role.name.clone();
    role.into_active_model().delete(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::CacheRoleDelete,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
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
