/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::endpoints::membership::find_user_by_username;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::invite_policy::invitation_expiry;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Admin, Member, TeamScope};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_notify::{InvitationMail, InviteScope, generate_token};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamMemberItem {
    pub user: String,
    pub name: String,
    pub role: TeamRole,
    pub source: TeamMemberSource,
}

impl TeamMemberItem {
    fn new(member: MUser, membership: MTeamUser) -> Self {
        Self {
            user: member.username,
            name: member.name,
            role: membership.role,
            source: membership.source,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct TeamMemberRequest {
    pub user: String,
    pub role: TeamRole,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RemoveTeamMemberRequest {
    pub user: String,
}

pub(crate) fn role_label(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Admin => "Admin",
        TeamRole::Member => "Member",
    }
}

fn ensure_not_state_managed(membership: &MTeamUser) -> WebResult<()> {
    if membership.source == TeamMemberSource::State {
        return Err(TeamRefusal::MemberManagedByState.into());
    }

    Ok(())
}

async fn ensure_not_last_admin(
    db: &impl ConnectionTrait,
    team: TeamId,
    membership: &MTeamUser,
) -> WebResult<()> {
    if membership.role != TeamRole::Admin {
        return Ok(());
    }
    let admins = gradient_db::teams::members::admin_count(db, team).await?;
    if admins <= 1 {
        return Err(SharedRefusal::LastAdmin(Resource::Team).into());
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/teams/{team}/members",
    operation_id = "listTeamMembers",
    tag = "teams",
    summary = "List team members",
    params(TeamPath),
    responses((status = OK, description = "OK", body = Vec<TeamMemberItem>), WebError)
)]
pub async fn get_team_members(
    TeamScope { team, db, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<TeamMemberItem>>>> {
    let rows = gradient_db::teams::members::members_of(&db.web_db, team.id).await?;

    Ok(ok_json(
        rows.into_iter()
            .filter_map(|(membership, member)| {
                member.map(|member| TeamMemberItem::new(member, membership))
            })
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/teams/{team}/members",
    operation_id = "addTeamMember",
    tag = "teams",
    summary = "Invite team member",
    description = "Needs Admin in the team. The user joins after accepting the invitation (`Invitation sent`); a superuser adds the member directly (`User added`).",
    params(TeamPath),
    request_body = TeamMemberRequest,
    responses((status = OK, description = "Invitation sent, or member added when the caller is a superuser", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_team_members(
    TeamScope { team, user, .. }: TeamScope<Admin>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<TeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user_by_username(&state.web_db, &body.user).await?;

    if gradient_db::teams::members::membership(&state.web_db, team.id, target.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamMembership));
    }
    if gradient_db::teams::invitations::by_team_and_user(&state.web_db, team.id, target.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::Invitation));
    }

    let payload = serde_json::json!({
        "team_id": team.id.to_string(),
        "target_user_id": target.id.to_string(),
        "role": role_label(body.role),
    });

    if user.superuser {
        gradient_db::teams::members::add_member(
            &state.web_db,
            MTeamUser {
                id: TeamUserId::now_v7(),
                team: team.id,
                user: target.id,
                role: body.role,
                source: TeamMemberSource::Api,
            },
        )
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TeamMembership))?;
        audit
            .write(Action::TeamMemberAdd, EventOwner::default(), payload)
            .await;
        return Ok(ok_json("User added".to_string()));
    }

    let now = gradient_types::now();
    let token = generate_token();
    gradient_db::teams::invitations::create(
        &state.web_db,
        MTeamInvitation {
            id: TeamInvitationId::now_v7(),
            team: team.id,
            user: target.id,
            role: body.role,
            invited_by: user.id,
            token: token.clone(),
            created_at: now,
            expires_at: invitation_expiry(now),
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::Invitation))?;

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

    audit
        .write(Action::TeamInvitationCreate, EventOwner::default(), payload)
        .await;
    Ok(ok_json("Invitation sent".to_string()))
}

#[utoipa::path(
    patch,
    path = "/teams/{team}/members",
    operation_id = "patchTeamMember",
    tag = "teams",
    summary = "Change team member role",
    description = "Needs Admin in the team.",
    params(TeamPath),
    request_body = TeamMemberRequest,
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_team_members(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    audit: Audit,
    Json(body): Json<TeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user_by_username(&db.web_db, &body.user).await?;
    let tx = db.web_db.inner().begin().await?;
    if body.role != TeamRole::Admin {
        gradient_db::teams::lock_for_admin_change(&tx, team.id).await?;
    }
    let membership = gradient_db::teams::members::membership(&tx, team.id, target.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Team))?;
    ensure_not_state_managed(&membership)?;

    if body.role != TeamRole::Admin {
        ensure_not_last_admin(&tx, team.id, &membership).await?;
    }

    let mut active: ATeamUser = membership.into();
    active.role = Set(body.role);
    if body.role == TeamRole::Admin {
        active.source = Set(TeamMemberSource::Api);
    }
    gradient_db::teams::members::update_member(&tx, active).await?;
    tx.commit().await?;

    audit.write(Action::TeamMemberRoleChange, EventOwner::default(), serde_json::json!({ "team_id": team.id.to_string(), "target_user_id": target.id.to_string(), "role": role_label(body.role), }))
    .await;

    Ok(ok_json("User role updated".to_string()))
}

#[utoipa::path(
    delete,
    path = "/teams/{team}/members",
    operation_id = "removeTeamMember",
    tag = "teams",
    summary = "Remove team member",
    description = "Needs Admin in the team.",
    params(TeamPath),
    request_body = RemoveTeamMemberRequest,
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_team_members(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    audit: Audit,
    Json(body): Json<RemoveTeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user_by_username(&db.web_db, &body.user).await?;
    let tx = db.web_db.inner().begin().await?;
    gradient_db::teams::lock_for_admin_change(&tx, team.id).await?;
    let membership = gradient_db::teams::members::membership(&tx, team.id, target.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Team))?;
    ensure_not_state_managed(&membership)?;

    ensure_not_last_admin(&tx, team.id, &membership).await?;
    gradient_db::teams::members::remove_member(&tx, membership).await?;
    tx.commit().await?;

    audit.write(Action::TeamMemberRemove, EventOwner::default(), serde_json::json!({ "team_id": team.id.to_string(), "target_user_id": target.id.to_string(), }))
    .await;

    Ok(ok_json("User removed".to_string()))
}
