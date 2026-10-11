/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{ProjectScope, Requires, RequiresUnmanaged, can};
use axum::Json;
use chrono::NaiveDateTime;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};

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
    ProjectScope { project, db, .. }: ProjectScope<Requires<can::ManageMembers>>,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let rows = EProjectInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::project_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CProjectInvitation::Project.eq(project.id))
        .all(&db.web_db)
        .await?;

    let role_ids: Vec<RoleId> = rows.iter().map(|(inv, _)| inv.role).collect();
    let role_map = role_names(&db.web_db, role_ids).await?;

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
    ProjectScope {
        project, user, db, ..
    }: ProjectScope<RequiresUnmanaged<can::ManageMembers>>,
    info: RequestInfo,
    Json(body): Json<RevokeInvitationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::User)?;

    let invitation = EProjectInvitation::find()
        .filter(
            Condition::all()
                .add(CProjectInvitation::Project.eq(project.id))
                .add(CProjectInvitation::User.eq(target_user.id)),
        )
        .one(&db.web_db)
        .await?
        .ok_or(SharedRefusal::NoPendingInvitation)?;

    invitation.into_active_model().delete(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::ProjectInvitationRevoke,
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

    Ok(ok_json("Invitation revoked".to_string()))
}
