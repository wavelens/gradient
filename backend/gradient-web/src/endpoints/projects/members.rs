/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringListItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddUserRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveUserRequest {
    pub user: String,
}

// ── Access helpers ────────────────────────────────────────────────────────────

async fn find_user_by_username(state: &Arc<ServerState>, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(&state.web_db)
        .await?
        .or_not_found("User")
}

async fn find_project_membership(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    user_id: UserId,
) -> WebResult<Option<MProjectUser>> {
    Ok(EProjectUser::find()
        .filter(
            Condition::all()
                .add(CProjectUser::Project.eq(project_id))
                .add(CProjectUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn get_project_users(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<StringListItem>>>> {
    let project = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable { label: "Project" },
    )
    .await?;

    let project_users = EProjectUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::project_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CProjectUser::Project.eq(project.id))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = project_users.iter().map(|(ou, _)| ou.role).collect();
    let role_map = role_names(&state.web_db, role_ids).await?;

    let items: Vec<StringListItem> = project_users
        .iter()
        .map(|(ou, user)| StringListItem {
            id: user
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| ou.user.to_string()),
            name: role_map
                .get(&ou.role)
                .cloned()
                .unwrap_or_else(|| ou.role.to_string()),
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn post_project_users(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    if find_project_membership(&state, project.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists("User already in Project"));
    }

    let role = ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(body.role.clone())).add(
                Condition::any()
                    .add(CRole::Project.eq(project.id))
                    .add(CRole::Project.is_null()),
            ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    MProjectUser {
        id: ProjectUserId::now_v7(),
        project: project.id,
        user: target_user.id,
        role: role.id,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::PROJECT_MEMBER_ADD,
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "role": role.name,
        })),
    )
    .await;

    Ok(ok_json("User invited".to_string()))
}

pub async fn patch_project_users(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_project_membership(&state, project.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Project"))?;

    let previous_role_id = membership.role;
    let role = ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(body.role.clone())).add(
                Condition::any()
                    .add(CRole::Project.eq(project.id))
                    .add(CRole::Project.is_null()),
            ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?;

    let mut active: AProjectUser = membership.into();
    active.role = Set(role.id);
    active.update(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::PROJECT_MEMBER_ROLE_CHANGE,
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "previous_role_id": previous_role_id.to_string(),
            "new_role": role.name,
        })),
    )
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_project_users(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<RemoveUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        },
    )
    .await?;
    let target_user = find_user_by_username(&state, &body.user).await?;

    let membership = find_project_membership(&state, project.id, target_user.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Project"))?;

    let active: AProjectUser = membership.into();
    active.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::PROJECT_MEMBER_REMOVE,
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("User kicked".to_string()))
}
