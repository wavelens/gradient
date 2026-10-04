/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{
    Permission, PermissionEntry, available_permissions, is_builtin_role, mask_to_vec,
    parse_permission_list,
};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::input::check_index_name;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Debug)]
pub struct RoleResponse {
    pub id: RoleId,
    pub name: String,
    pub project: Option<ProjectId>,
    pub builtin: bool,
    pub managed: bool,
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
            project: role.project,
            builtin,
            managed: role.managed,
            permissions,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct RoleListResponse {
    pub roles: Vec<RoleResponse>,
    pub available_permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize, Debug)]
pub struct CreateRoleRequest {
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct PatchRoleRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
}

async fn load_project_role(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    role_id: RoleId,
) -> WebResult<MRole> {
    let role = ERole::find_by_id(role_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    if let Some(owner) = role.project
        && owner != project_id
    {
        // Cross-project access is treated as not-found to avoid leaking ids.
        return Err(WebError::not_found("Role"));
    }

    Ok(role)
}

pub async fn get_project_roles(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<RoleListResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let roles = ERole::find()
        .filter(
            Condition::any()
                .add(CRole::Project.is_null())
                .add(CRole::Project.eq(project.id)),
        )
        .all(&state.web_db)
        .await?;

    Ok(ok_json(RoleListResponse {
        roles: roles.into_iter().map(RoleResponse::from_model).collect(),
        available_permissions: available_permissions(),
    }))
}

pub async fn post_project_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<CreateRoleRequest>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    if check_index_name(&body.name).is_err() {
        return Err(WebError::invalid_name("Role Name"));
    }

    let mask = parse_permission_list(&body.permissions, "GET /projects/{project}/roles")?;

    // Names must be unique within the project and must not collide with a built-in role name.
    // Membership lookup by name would become ambiguous otherwise.
    let clash = ERole::find()
        .filter(CRole::Name.eq(body.name.as_str()))
        .filter(
            Condition::any()
                .add(CRole::Project.eq(project.id))
                .add(CRole::Project.is_null()),
        )
        .one(&state.web_db)
        .await?;
    if clash.is_some() {
        return Err(WebError::already_exists("Role Name"));
    }

    let role = MRole {
        id: RoleId::now_v7(),
        name: body.name.clone(),
        project: Some(project.id),
        permission: mask,
        ..Default::default()
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state,
        Some(user.id),
        Action::ProjectRoleCreate,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "role_id": role.id.to_string(),
            "name": role.name,
            "permission_mask": mask,
        })),
    )
    .await;

    Ok(ok_json(RoleResponse::from_model(role)))
}

pub async fn get_project_role(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;
    let role = load_project_role(&state, project.id, role_id).await?;
    Ok(ok_json(RoleResponse::from_model(role)))
}

pub async fn patch_project_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, role_id)): Path<(String, RoleId)>,
    Json(body): Json<PatchRoleRequest>,
) -> WebResult<Json<BaseResponse<RoleResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_project_role(&state, project.id, role_id).await?;

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
                    .add(CRole::Project.eq(project.id))
                    .add(CRole::Project.is_null()),
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
            "GET /projects/{project}/roles",
        )?);
    }

    let updated = active.update(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::ProjectRoleUpdate,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
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

pub async fn delete_project_role(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, role_id)): Path<(String, RoleId)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageRoles,
            reject_managed: true,
        },
    )
    .await?;

    let role = load_project_role(&state, project.id, role_id).await?;

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

    let in_use = EProjectUser::find()
        .filter(CProjectUser::Role.eq(role_id))
        .filter(CProjectUser::Project.eq(project.id))
        .one(&state.web_db)
        .await?
        .is_some();
    if in_use {
        return Err(WebError::bad_request(
            "Role is still assigned to members. Reassign them before deleting the role.",
        ));
    }
    if gradient_db::teams::grants::project_role_granted_to_a_team(&state.web_db, role_id).await? {
        return Err(WebError::bad_request(
            "Role is still granted to a team. Change the team grant before deleting the role.",
        ));
    }

    let role_name = role.name.clone();
    role.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::ProjectRoleDelete,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "role_id": role_id.to_string(),
            "name": role_name,
        })),
    )
    .await;

    Ok(ok_json(true))
}
