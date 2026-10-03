/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::members::role_label;
use crate::access::{TeamAccess, load_team};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::endpoints::projects::invitations::{PendingInvitationItem, RevokeInvitationRequest};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
};
use std::sync::Arc;

pub async fn get_team_invitations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let (team, _) = load_team(
        &state,
        &user,
        api_key.as_ref(),
        team,
        TeamAccess::Admin {
            reject_managed: false,
        },
    )
    .await?;

    let rows = ETeamInvitation::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_invitation::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CTeamInvitation::Team.eq(team.id))
        .all(&state.web_db)
        .await?;

    let items: Vec<PendingInvitationItem> = rows
        .iter()
        .map(|(inv, invitee)| PendingInvitationItem {
            user: invitee
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| inv.user.to_string()),
            name: invitee.as_ref().map(|u| u.name.clone()).unwrap_or_default(),
            role: role_label(inv.role).to_string(),
            created_at: inv.created_at,
            expires_at: inv.expires_at,
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn delete_team_invitation(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
    Json(body): Json<RevokeInvitationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (team, _) = load_team(
        &state,
        &user,
        api_key.as_ref(),
        team,
        TeamAccess::Admin {
            reject_managed: true,
        },
    )
    .await?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.web_db)
        .await?
        .or_not_found("User")?;

    let invitation = ETeamInvitation::find()
        .filter(CTeamInvitation::Team.eq(team.id))
        .filter(CTeamInvitation::User.eq(target_user.id))
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::bad_request("No pending invitation for this user"))?;

    invitation.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamInvitationRevoke,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({
            "team_id": team.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
