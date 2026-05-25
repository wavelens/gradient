/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD for organization-scoped custom roles.
//!
//! Each org carries the three immutable built-in roles (Admin/Write/View) for
//! free; on top of that, users with [`Permission::ManageRoles`] can mint
//! custom roles whose permission set is freely chosen from
//! [`Permission::ALL`]. Custom roles live under `role.organization = <org_id>`
//! and are tagged `builtin: false` in API responses.

use crate::access::{Caller, OrgAccess, load_org};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{
    Permission, PermissionEntry, available_permissions, is_builtin_role, mask_to_vec,
    parse_permission_list,
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
pub struct RoleResponse {
    pub id: RoleId,
    pub name: String,
    /// `null` for built-in roles, the org id for custom roles.
    pub organization: Option<OrganizationId>,
    /// True for the three immutable system roles (Admin/Write/View).
    pub builtin: bool,
    /// True for roles provisioned from `gradient-state.nix`. Managed roles
    /// are immutable through this API (the same way built-in roles are).
    pub managed: bool,
    /// Capability identifiers (camelCase) granted by this role; matches the
    /// strings produced by [`Permission::as_wire_name`].
    pub permissions: Vec<&'static str>,
}

impl RoleResponse {
    fn from_model(role: MRole) -> Self {
        let builtin = is_builtin_role(role.id);
        let permissions = mask_to_vec(role.permission)
            .into_iter()
            .map(|p| p.as_wire_name())
            .collect();
        Self {
            id: role.id,
            name: role.name,
            organization: role.organization,
            builtin,
            managed: role.managed,
            permissions,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct RoleListResponse {
    /// Roles available in this org: the three built-ins plus any custom
    /// roles owned by the org.
    pub roles: Vec<RoleResponse>,
    /// All capabilities a custom role may carry, for the role-management UI.
    pub available_permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize, Debug)]
pub struct CreateRoleRequest {
    pub name: String,
    /// Capability identifiers (matching [`Permission::as_wire_name`]) the
    /// new role should grant. Unknown identifiers are rejected.
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchRoleRequest {
    pub name: Option<String>,
    /// When present, replaces the role's permissions wholesale.
    pub permissions: Option<Vec<String>>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn load_org_role(
    state: &Arc<ServerState>,
    org_id: OrganizationId,
    role_id: RoleId,
) -> WebResult<MRole> {
    let role = ERole::find_by_id(role_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    if let Some(owner) = role.organization
        && owner != org_id
    {
        // Treat cross-org access as not-found to avoid leaking ids.
        return Err(WebError::not_found("Role"));
    }

    Ok(role)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /orgs/{organization}/roles` - list roles available in the org.
///
/// Visible to any member (so the add-member UI can populate its role
/// dropdown). The `available_permissions` catalogue is included on every
/// response for the role-management UI.
pub async fn get_organization_roles(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<RoleListResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let roles = ERole::find()
        .filter(
            Condition::any()
                .add(CRole::Organization.is_null())
                .add(CRole::Organization.eq(org.id)),
        )
        .all(&state.web_db)
        .await?;

    Ok(ok_json(RoleListResponse {
        roles: roles.into_iter().map(RoleResponse::from_model).collect(),
        available_permissions: available_permissions(),
    }))
}

/// `POST /orgs/{organization}/roles` - create a custom role.
pub async fn post_organization_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
    Json(body): Json<CreateRoleRequest>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name("Role Name"));
    }

    let mask = parse_permission_list(&body.permissions, "GET /orgs/{organization}/roles")?;

    // Names must be unique within (org_id, name) and must not collide with a
    // built-in role's name (Admin/Write/View) - otherwise membership lookup
    // by name becomes ambiguous.
    let clash = ERole::find()
        .filter(CRole::Name.eq(body.name.as_str()))
        .filter(
            Condition::any()
                .add(CRole::Organization.eq(org.id))
                .add(CRole::Organization.is_null()),
        )
        .one(&state.web_db)
        .await?;
    if clash.is_some() {
        return Err(WebError::already_exists("Role Name"));
    }

    let role = ARole {
        id: Set(RoleId::now_v7()),
        name: Set(body.name.clone()),
        organization: Set(Some(org.id)),
        permission: Set(mask),
        managed: Set(false),
    }
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::ORG_ROLE_CREATE,
        &info,
        Some(serde_json::json!({
            "organization_id": org.id.to_string(),
            "role_id": role.id.to_string(),
            "name": role.name,
            "permission_mask": mask,
        })),
    )
    .await;

    Ok(ok_json(RoleResponse::from_model(role)))
}

/// `GET /orgs/{organization}/roles/{role_id}` - fetch a single role.
pub async fn get_organization_role(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;
    let role = load_org_role(&state, org.id, role_id).await?;
    Ok(ok_json(RoleResponse::from_model(role)))
}

/// `PATCH /orgs/{organization}/roles/{role_id}` - update a custom role.
///
/// Built-in roles are immutable: attempting to mutate them returns 403.
pub async fn patch_organization_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, role_id)): Path<(String, RoleId)>,
    Json(body): Json<PatchRoleRequest>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_org_role(&state, org.id, role_id).await?;

    if is_builtin_role(role.id) {
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
    let mut active: ARole = role.into_active_model();

    if let Some(name) = body.name {
        if check_index_name(&name).is_err() {
            return Err(WebError::invalid_name("Role Name"));
        }
        let clash = ERole::find()
            .filter(CRole::Name.eq(name.as_str()))
            .filter(CRole::Id.ne(role_id))
            .filter(
                Condition::any()
                    .add(CRole::Organization.eq(org.id))
                    .add(CRole::Organization.is_null()),
            )
            .one(&state.web_db)
            .await?;
        if clash.is_some() {
            return Err(WebError::already_exists("Role Name"));
        }
        active.name = Set(name);
    }

    if let Some(perms) = body.permissions {
        active.permission = Set(parse_permission_list(
            &perms,
            "GET /orgs/{organization}/roles",
        )?);
    }

    let updated = active.update(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::ORG_ROLE_UPDATE,
        &info,
        Some(serde_json::json!({
            "organization_id": org.id.to_string(),
            "role_id": updated.id.to_string(),
            "previous_name": previous_name,
            "previous_permission_mask": previous_mask,
            "new_name": updated.name,
            "new_permission_mask": updated.permission,
        })),
    )
    .await;

    Ok(ok_json(RoleResponse::from_model(updated)))
}

/// `DELETE /orgs/{organization}/roles/{role_id}` - delete a custom role.
///
/// Refuses to delete a role that is still in use; the caller must reassign
/// affected members first (the UI surfaces the in-use count).
pub async fn delete_organization_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_org_role(&state, org.id, role_id).await?;

    if is_builtin_role(role.id) {
        return Err(WebError::forbidden(
            "Built-in roles (Admin, Write, View) cannot be deleted.",
        ));
    }

    if role.managed {
        return Err(WebError::forbidden(
            "State-managed roles cannot be deleted via the API.",
        ));
    }

    let in_use = EOrganizationUser::find()
        .filter(COrganizationUser::Role.eq(role_id))
        .filter(COrganizationUser::Organization.eq(org.id))
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
        events::ORG_ROLE_DELETE,
        &info,
        Some(serde_json::json!({
            "organization_id": org.id.to_string(),
            "role_id": role_id.to_string(),
            "name": role_name,
        })),
    )
    .await;

    Ok(ok_json(true))
}
