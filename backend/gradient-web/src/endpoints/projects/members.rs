/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::invite_policy::invitation_expiry;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{ProjectScope, Readable, RequiresUnmanaged, can};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_notify::{InvitationMail, InviteScope, generate_token};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
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

async fn find_user_by_username(db: &WebDb, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(db)
        .await?
        .or_not_found(Resource::User)
}

async fn find_project_membership(
    db: &WebDb,
    project_id: ProjectId,
    user_id: UserId,
) -> WebResult<Option<MProjectUser>> {
    Ok(EProjectUser::find()
        .filter(
            Condition::all()
                .add(CProjectUser::Project.eq(project_id))
                .add(CProjectUser::User.eq(user_id)),
        )
        .one(db)
        .await?)
}

async fn find_project_invitation(
    db: &WebDb,
    project_id: ProjectId,
    user_id: UserId,
) -> WebResult<Option<MProjectInvitation>> {
    Ok(EProjectInvitation::find()
        .filter(
            Condition::all()
                .add(CProjectInvitation::Project.eq(project_id))
                .add(CProjectInvitation::User.eq(user_id)),
        )
        .one(db)
        .await?)
}

pub async fn get_project_users(
    ProjectScope { project, db, .. }: ProjectScope<Readable>,
) -> WebResult<Json<BaseResponse<Vec<StringListItem>>>> {
    let project_users = EProjectUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::project_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CProjectUser::Project.eq(project.id))
        .all(&db.web_db)
        .await?;

    let role_ids: Vec<RoleId> = project_users.iter().map(|(ou, _)| ou.role).collect();
    let role_map = role_names(&db.web_db, role_ids).await?;

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
    ProjectScope {
        project, user, db, ..
    }: ProjectScope<RequiresUnmanaged<can::ManageMembers>>,
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&state.web_db, &body.user).await?;

    if find_project_membership(&state.web_db, project.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::ProjectMembership));
    }

    if find_project_invitation(&state.web_db, project.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::Invitation));
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
        .or_not_found(Resource::Role)?;

    if user.superuser {
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
            &db,
            Some(user.id),
            Action::ProjectMemberAdd,
            EventOwner {
                project: Some(project.id),
                ..Default::default()
            },
            &info,
            Some(serde_json::json!({
                "project_id": project.id.to_string(),
                "target_user_id": target_user.id.to_string(),
                "role": role.name,
            })),
        )
        .await;

        return Ok(ok_json("User added".to_string()));
    }

    let now = gradient_types::now();
    let token = generate_token();

    MProjectInvitation {
        id: ProjectInvitationId::now_v7(),
        project: project.id,
        user: target_user.id,
        role: role.id,
        invited_by: user.id,
        token: token.clone(),
        created_at: now,
        expires_at: invitation_expiry(now),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    if state.email.is_enabled()
        && let Err(e) = state
            .email
            .send_invitation_email(
                &target_user.email,
                &target_user.name,
                &InvitationMail {
                    scope: InviteScope::Project,
                    scope_display_name: &project.display_name,
                    role: &role.name,
                    inviter: &user.username,
                    accept_url: format!(
                        "{}/settings/invites?token={}",
                        state.config.server.serve_url, token
                    ),
                },
            )
            .await
    {
        tracing::warn!(error = %e, "Failed to send project invitation email");
    }

    audit_record(
        &db,
        Some(user.id),
        Action::ProjectInvitationCreate,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "role": role.name,
        })),
    )
    .await;

    Ok(ok_json("Invitation sent".to_string()))
}

pub async fn patch_project_users(
    ProjectScope {
        project, user, db, ..
    }: ProjectScope<RequiresUnmanaged<can::ManageMembers>>,
    info: RequestInfo,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&db.web_db, &body.user).await?;

    let membership = find_project_membership(&db.web_db, project.id, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Project))?;

    let previous_role_id = membership.role;
    let role = ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(body.role.clone())).add(
                Condition::any()
                    .add(CRole::Project.eq(project.id))
                    .add(CRole::Project.is_null()),
            ),
        )
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Role)?;

    let mut active: AProjectUser = membership.into();
    active.role = Set(role.id);
    active.update(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::ProjectMemberRoleChange,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
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
    ProjectScope {
        project, user, db, ..
    }: ProjectScope<RequiresUnmanaged<can::ManageMembers>>,
    info: RequestInfo,
    Json(body): Json<RemoveUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&db.web_db, &body.user).await?;

    let membership = find_project_membership(&db.web_db, project.id, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Project))?;

    let active: AProjectUser = membership.into();
    active.delete(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::ProjectMemberRemove,
        EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("User kicked".to_string()))
}
