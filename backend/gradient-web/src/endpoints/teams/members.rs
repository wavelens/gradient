/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{TeamAccess, load_team};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::invite_policy::invitation_expiry;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_notify::{InvitationMail, InviteScope, generate_token};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, JoinType, PaginatorTrait,
    QueryFilter, QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub struct TeamMemberItem {
    pub user: String,
    pub name: String,
    pub role: TeamRole,
    pub via_group: bool,
}

#[derive(Deserialize)]
pub struct TeamMemberRequest {
    pub user: String,
    pub role: TeamRole,
}

#[derive(Deserialize)]
pub struct RemoveTeamMemberRequest {
    pub user: String,
}

pub(crate) fn role_label(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Admin => "Admin",
        TeamRole::Member => "Member",
    }
}

async fn find_user(state: &Arc<ServerState>, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(&state.web_db)
        .await?
        .or_not_found("User")
}

async fn find_membership(
    state: &Arc<ServerState>,
    team: TeamId,
    user: UserId,
) -> WebResult<Option<MTeamUser>> {
    Ok(ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .one(&state.web_db)
        .await?)
}

async fn ensure_not_last_admin(
    state: &Arc<ServerState>,
    team: TeamId,
    membership: &MTeamUser,
) -> WebResult<()> {
    if membership.role != TeamRole::Admin {
        return Ok(());
    }
    let admins = ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::Role.eq(TeamRole::Admin))
        .count(&state.web_db)
        .await?;
    if admins <= 1 {
        return Err(WebError::conflict(
            "Cannot remove the last Admin from the team.",
        ));
    }
    Ok(())
}

pub async fn get_team_members(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<TeamMemberItem>>>> {
    let (team, _) = load_team(&state, &user, api_key.as_ref(), team, TeamAccess::Member).await?;

    let rows = ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CTeamUser::Team.eq(team.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter()
            .filter_map(|(membership, member)| {
                member.map(|member| TeamMemberItem {
                    user: member.username,
                    name: member.name,
                    role: membership.role,
                    via_group: membership.via_group,
                })
            })
            .collect(),
    ))
}

pub async fn post_team_members(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
    Json(body): Json<TeamMemberRequest>,
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
    let target = find_user(&state, &body.user).await?;

    if find_membership(&state, team.id, target.id).await?.is_some() {
        return Err(WebError::already_exists("User already in Team"));
    }
    if ETeamInvitation::find()
        .filter(CTeamInvitation::Team.eq(team.id))
        .filter(CTeamInvitation::User.eq(target.id))
        .one(&state.web_db)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists("User already invited"));
    }

    let payload = serde_json::json!({
        "team_id": team.id.to_string(),
        "target_user_id": target.id.to_string(),
        "role": role_label(body.role),
    });

    if user.superuser {
        MTeamUser {
            id: TeamUserId::now_v7(),
            team: team.id,
            user: target.id,
            role: body.role,
            via_group: false,
        }
        .into_active_model()
        .insert(&state.web_db)
        .await?;
        audit_record(
            &state,
            Some(user.id),
            Action::TeamMemberAdd,
            EventOwner::default(),
            &info,
            Some(payload),
        )
        .await;
        return Ok(ok_json("User added".to_string()));
    }

    let now = gradient_types::now();
    let token = generate_token();
    MTeamInvitation {
        id: TeamInvitationId::now_v7(),
        team: team.id,
        user: target.id,
        role: body.role,
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
                &target.email,
                &target.name,
                &InvitationMail {
                    scope: InviteScope::Team,
                    scope_display_name: &team.display_name,
                    role: role_label(body.role),
                    inviter: &user.username,
                    accept_url: format!(
                        "{}/settings/invites?token={}",
                        state.config.server.serve_url, token
                    ),
                },
            )
            .await
    {
        tracing::warn!(error = %e, "Failed to send team invitation email");
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamInvitationCreate,
        EventOwner::default(),
        &info,
        Some(payload),
    )
    .await;
    Ok(ok_json("Invitation sent".to_string()))
}

pub async fn patch_team_members(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
    Json(body): Json<TeamMemberRequest>,
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
    let target = find_user(&state, &body.user).await?;
    let membership = find_membership(&state, team.id, target.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Team"))?;

    if body.role != TeamRole::Admin {
        ensure_not_last_admin(&state, team.id, &membership).await?;
    }

    let mut active: ATeamUser = membership.into();
    active.role = Set(body.role);
    active.update(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamMemberRoleChange,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({
            "team_id": team.id.to_string(),
            "target_user_id": target.id.to_string(),
            "role": role_label(body.role),
        })),
    )
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_team_members(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
    Json(body): Json<RemoveTeamMemberRequest>,
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
    let target = find_user(&state, &body.user).await?;
    let membership = find_membership(&state, team.id, target.id)
        .await?
        .ok_or_else(|| WebError::bad_request("User not in Team"))?;

    ensure_not_last_admin(&state, team.id, &membership).await?;
    membership.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamMemberRemove,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({
            "team_id": team.id.to_string(),
            "target_user_id": target.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("User removed".to_string()))
}
