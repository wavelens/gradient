/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::invite_policy::invitation_expiry;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Admin, Member, TeamScope};
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
    pub source: TeamMemberSource,
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

async fn find_user(db: &WebDb, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(db)
        .await?
        .or_not_found(Resource::User)
}

async fn find_membership(db: &WebDb, team: TeamId, user: UserId) -> WebResult<Option<MTeamUser>> {
    Ok(ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .one(db)
        .await?)
}

fn ensure_not_state_managed(membership: &MTeamUser) -> WebResult<()> {
    if membership.source == TeamMemberSource::State {
        return Err(TeamRefusal::MemberManagedByState.into());
    }

    Ok(())
}

async fn ensure_not_last_admin(db: &WebDb, team: TeamId, membership: &MTeamUser) -> WebResult<()> {
    if membership.role != TeamRole::Admin {
        return Ok(());
    }
    let admins = ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::Role.eq(TeamRole::Admin))
        .count(db)
        .await?;
    if admins <= 1 {
        return Err(SharedRefusal::LastAdmin(Resource::Team).into());
    }
    Ok(())
}

pub async fn get_team_members(
    TeamScope { team, db, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<TeamMemberItem>>>> {
    let rows = ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CTeamUser::Team.eq(team.id))
        .all(&db.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter()
            .filter_map(|(membership, member)| {
                member.map(|member| TeamMemberItem {
                    user: member.username,
                    name: member.name,
                    role: membership.role,
                    source: membership.source,
                })
            })
            .collect(),
    ))
}

pub async fn post_team_members(
    TeamScope { team, user, .. }: TeamScope<Admin>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<TeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user(&state.web_db, &body.user).await?;

    if find_membership(&state.web_db, team.id, target.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamMembership));
    }
    if ETeamInvitation::find()
        .filter(CTeamInvitation::Team.eq(team.id))
        .filter(CTeamInvitation::User.eq(target.id))
        .one(&state.web_db)
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
        MTeamUser {
            id: TeamUserId::now_v7(),
            team: team.id,
            user: target.id,
            role: body.role,
            source: TeamMemberSource::Api,
        }
        .into_active_model()
        .insert(&state.web_db)
        .await?;
        audit
            .write(Action::TeamMemberAdd, EventOwner::default(), payload)
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

    audit
        .write(Action::TeamInvitationCreate, EventOwner::default(), payload)
        .await;
    Ok(ok_json("Invitation sent".to_string()))
}

pub async fn patch_team_members(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    audit: Audit,
    Json(body): Json<TeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user(&db.web_db, &body.user).await?;
    let membership = find_membership(&db.web_db, team.id, target.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Team))?;
    ensure_not_state_managed(&membership)?;

    if body.role != TeamRole::Admin {
        ensure_not_last_admin(&db.web_db, team.id, &membership).await?;
    }

    let mut active: ATeamUser = membership.into();
    active.role = Set(body.role);
    if body.role == TeamRole::Admin {
        active.source = Set(TeamMemberSource::Api);
    }
    active.update(&db.web_db).await?;

    audit.write(Action::TeamMemberRoleChange, EventOwner::default(), serde_json::json!({ "team_id": team.id.to_string(), "target_user_id": target.id.to_string(), "role": role_label(body.role), }))
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_team_members(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    audit: Audit,
    Json(body): Json<RemoveTeamMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = find_user(&db.web_db, &body.user).await?;
    let membership = find_membership(&db.web_db, team.id, target.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Team))?;
    ensure_not_state_managed(&membership)?;

    ensure_not_last_admin(&db.web_db, team.id, &membership).await?;
    membership.into_active_model().delete(&db.web_db).await?;

    audit.write(Action::TeamMemberRemove, EventOwner::default(), serde_json::json!({ "team_id": team.id.to_string(), "target_user_id": target.id.to_string(), }))
    .await;

    Ok(ok_json("User removed".to_string()))
}
