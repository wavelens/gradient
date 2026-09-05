/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInvitationItem {
    pub user: String,
    pub name: String,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RevokeInvitationRequest {
    pub user: String,
}

pub async fn get_project_invitations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: false,
        },
    )
    .await?;

    let rows = EProjectInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::project_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CProjectInvitation::Project.eq(project.id))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = rows.iter().map(|(inv, _)| inv.role).collect();
    let role_map = role_names(&state.web_db, role_ids).await?;

    let items: Vec<PendingInvitationItem> = rows
        .iter()
        .map(|(inv, invitee)| PendingInvitationItem {
            user: invitee
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| inv.user.to_string()),
            name: invitee.as_ref().map(|u| u.name.clone()).unwrap_or_default(),
            role: role_map
                .get(&inv.role)
                .cloned()
                .unwrap_or_else(|| inv.role.to_string()),
            created_at: inv.created_at,
            expires_at: inv.expires_at,
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn delete_project_invitation(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
    Json(body): Json<RevokeInvitationRequest>,
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

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.web_db)
        .await?
        .or_not_found("User")?;

    let invitation = EProjectInvitation::find()
        .filter(
            Condition::all()
                .add(CProjectInvitation::Project.eq(project.id))
                .add(CProjectInvitation::User.eq(target_user.id)),
        )
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::bad_request("No pending invitation for this user"))?;

    invitation.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::PROJECT_INVITATION_REVOKE,
        &info,
        Some(serde_json::json!({
            "project_id": project.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
