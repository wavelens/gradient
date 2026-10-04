/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::endpoints::teams::members::role_label;
use crate::error::{WebError, WebResult};
use crate::helpers::{ok_json, role_names};
use crate::invite_policy::{
    InviteDecision, InviteItem, InviteKind, evaluate_invite, merge_invites,
};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct InviteTokenRequest {
    pub token: String,
}

pub async fn get_user_invites(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<InviteItem>>>> {
    let project_rows = EProjectInvitation::find()
        .filter(CProjectInvitation::User.eq(user.id))
        .all(&state.web_db)
        .await?;
    let cache_rows = ECacheInvitation::find()
        .filter(CCacheInvitation::User.eq(user.id))
        .all(&state.web_db)
        .await?;
    let team_rows = ETeamInvitation::find()
        .filter(CTeamInvitation::User.eq(user.id))
        .all(&state.web_db)
        .await?;

    let inviter_ids: Vec<UserId> = project_rows
        .iter()
        .map(|r| r.invited_by)
        .chain(cache_rows.iter().map(|r| r.invited_by))
        .chain(team_rows.iter().map(|r| r.invited_by))
        .collect();
    let inviters: HashMap<UserId, String> = EUser::find()
        .filter(CUser::Id.is_in(inviter_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|u| (u.id, u.username))
        .collect();

    let projects: HashMap<ProjectId, MProject> = EProject::find()
        .filter(CProject::Id.is_in(project_rows.iter().map(|r| r.project).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let caches: HashMap<CacheId, MCache> = ECache::find()
        .filter(CCache::Id.is_in(cache_rows.iter().map(|r| r.cache).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let teams: HashMap<TeamId, MTeam> = ETeam::find()
        .filter(CTeam::Id.is_in(team_rows.iter().map(|r| r.team).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();

    let project_roles = role_names(
        &state.web_db,
        project_rows.iter().map(|r| r.role).collect::<Vec<_>>(),
    )
    .await?;
    let cache_roles: HashMap<RoleId, String> = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(cache_rows.iter().map(|r| r.role).collect::<Vec<_>>()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let project_items: Vec<InviteItem> = project_rows
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            Some(InviteItem {
                kind: InviteKind::Project,
                token: r.token.clone(),
                scope: project.name.clone(),
                scope_display_name: project.display_name.clone(),
                role: project_roles
                    .get(&r.role)
                    .cloned()
                    .unwrap_or_else(|| r.role.to_string()),
                invited_by: inviters
                    .get(&r.invited_by)
                    .cloned()
                    .unwrap_or_else(|| r.invited_by.to_string()),
                created_at: r.created_at,
                expires_at: r.expires_at,
            })
        })
        .collect();

    let cache_items: Vec<InviteItem> = cache_rows
        .iter()
        .filter_map(|r| {
            let cache = caches.get(&r.cache)?;
            Some(InviteItem {
                kind: InviteKind::Cache,
                token: r.token.clone(),
                scope: cache.name.clone(),
                scope_display_name: cache.display_name.clone(),
                role: cache_roles
                    .get(&r.role)
                    .cloned()
                    .unwrap_or_else(|| r.role.to_string()),
                invited_by: inviters
                    .get(&r.invited_by)
                    .cloned()
                    .unwrap_or_else(|| r.invited_by.to_string()),
                created_at: r.created_at,
                expires_at: r.expires_at,
            })
        })
        .collect();

    let team_items = team_rows.iter().filter_map(|r| {
        let team = teams.get(&r.team)?;
        Some(InviteItem {
            kind: InviteKind::Team,
            token: r.token.clone(),
            scope: team.name.clone(),
            scope_display_name: team.display_name.clone(),
            role: role_label(r.role).to_string(),
            invited_by: inviters
                .get(&r.invited_by)
                .cloned()
                .unwrap_or_else(|| r.invited_by.to_string()),
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
    });

    Ok(ok_json(merge_invites(
        project_items
            .into_iter()
            .chain(cache_items)
            .chain(team_items)
            .collect(),
    )))
}

enum Invitation {
    Project(MProjectInvitation),
    Cache(MCacheInvitation),
    Team(MTeamInvitation),
}

impl Invitation {
    fn invitee(&self) -> UserId {
        match self {
            Self::Project(i) => i.user,
            Self::Cache(i) => i.user,
            Self::Team(i) => i.user,
        }
    }

    fn expires_at(&self) -> chrono::NaiveDateTime {
        match self {
            Self::Project(i) => i.expires_at,
            Self::Cache(i) => i.expires_at,
            Self::Team(i) => i.expires_at,
        }
    }
}

async fn find_invitation(state: &Arc<ServerState>, token: &str) -> WebResult<Invitation> {
    if let Some(row) = EProjectInvitation::find()
        .filter(CProjectInvitation::Token.eq(token))
        .one(&state.web_db)
        .await?
    {
        return Ok(Invitation::Project(row));
    }

    if let Some(row) = ECacheInvitation::find()
        .filter(CCacheInvitation::Token.eq(token))
        .one(&state.web_db)
        .await?
    {
        return Ok(Invitation::Cache(row));
    }

    ETeamInvitation::find()
        .filter(CTeamInvitation::Token.eq(token))
        .one(&state.web_db)
        .await?
        .map(Invitation::Team)
        .ok_or_else(|| WebError::not_found("Invitation"))
}

/// The session must belong to the invitee. A forwarded mail is useless to anybody else.
async fn claim_invitation(
    state: &Arc<ServerState>,
    user: &MUser,
    token: &str,
) -> WebResult<Invitation> {
    let invitation = find_invitation(state, token).await?;

    match evaluate_invite(
        invitation.invitee(),
        user.id,
        invitation.expires_at(),
        gradient_types::now(),
    ) {
        InviteDecision::Redeem => Ok(invitation),
        InviteDecision::NotInvitee => Err(WebError::not_found("Invitation")),
        InviteDecision::Expired => {
            match invitation {
                Invitation::Project(i) => {
                    i.into_active_model().delete(&state.web_db).await?;
                }
                Invitation::Cache(i) => {
                    i.into_active_model().delete(&state.web_db).await?;
                }
                Invitation::Team(i) => {
                    i.into_active_model().delete(&state.web_db).await?;
                }
            }

            Err(WebError::gone("Invitation has expired"))
        }
    }
}

pub async fn post_accept_invite(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Json(body): Json<InviteTokenRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let invitation = claim_invitation(&state, &user, &body.token).await?;
    let tx = state.web_db.inner().begin().await?;

    let (action, owner, payload) = match invitation {
        Invitation::Project(inv) => {
            let already = EProjectUser::find()
                .filter(CProjectUser::Project.eq(inv.project))
                .filter(CProjectUser::User.eq(inv.user))
                .one(&tx)
                .await?
                .is_some();

            let payload = serde_json::json!({
                "project_id": inv.project.to_string(),
                "role_id": inv.role.to_string(),
            });
            let project = inv.project;
            let role = inv.role;
            let member = inv.user;
            inv.into_active_model().delete(&tx).await?;

            if !already {
                MProjectUser {
                    id: ProjectUserId::now_v7(),
                    project,
                    user: member,
                    role,
                }
                .into_active_model()
                .insert(&tx)
                .await?;
            }

            (
                Action::ProjectInvitationAccept,
                EventOwner {
                    project: Some(project),
                    ..Default::default()
                },
                payload,
            )
        }
        Invitation::Cache(inv) => {
            let already = ECacheUser::find()
                .filter(CCacheUser::Cache.eq(inv.cache))
                .filter(CCacheUser::User.eq(inv.user))
                .one(&tx)
                .await?
                .is_some();

            let payload = serde_json::json!({
                "cache_id": inv.cache.to_string(),
                "role_id": inv.role.to_string(),
            });
            let cache = inv.cache;
            let role = inv.role;
            let member = inv.user;
            inv.into_active_model().delete(&tx).await?;

            if !already {
                MCacheUser {
                    id: CacheUserId::now_v7(),
                    cache,
                    user: member,
                    role,
                }
                .into_active_model()
                .insert(&tx)
                .await?;
            }

            (
                Action::CacheInvitationAccept,
                EventOwner {
                    cache: Some(cache),
                    ..Default::default()
                },
                payload,
            )
        }
        Invitation::Team(inv) => {
            let already = ETeamUser::find()
                .filter(CTeamUser::Team.eq(inv.team))
                .filter(CTeamUser::User.eq(inv.user))
                .one(&tx)
                .await?
                .is_some();

            let payload = serde_json::json!({ "team_id": inv.team.to_string() });
            let (team, role, member) = (inv.team, inv.role, inv.user);
            inv.into_active_model().delete(&tx).await?;

            if !already {
                MTeamUser {
                    id: TeamUserId::now_v7(),
                    team,
                    user: member,
                    role,
                    source: TeamMemberSource::Api,
                }
                .into_active_model()
                .insert(&tx)
                .await?;
            }

            (Action::TeamInvitationAccept, EventOwner::default(), payload)
        }
    };

    tx.commit().await?;
    audit_record(&state, Some(user.id), action, owner, &info, Some(payload)).await;

    Ok(ok_json("Invitation accepted".to_string()))
}

pub async fn post_decline_invite(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Json(body): Json<InviteTokenRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let invitation = claim_invitation(&state, &user, &body.token).await?;

    let (action, owner, payload) = match invitation {
        Invitation::Project(inv) => {
            let owner = EventOwner {
                project: Some(inv.project),
                ..Default::default()
            };
            let payload = serde_json::json!({ "project_id": inv.project.to_string() });
            inv.into_active_model().delete(&state.web_db).await?;
            (Action::ProjectInvitationDecline, owner, payload)
        }
        Invitation::Cache(inv) => {
            let owner = EventOwner {
                cache: Some(inv.cache),
                ..Default::default()
            };
            let payload = serde_json::json!({ "cache_id": inv.cache.to_string() });
            inv.into_active_model().delete(&state.web_db).await?;
            (Action::CacheInvitationDecline, owner, payload)
        }
        Invitation::Team(inv) => {
            let payload = serde_json::json!({ "team_id": inv.team.to_string() });
            inv.into_active_model().delete(&state.web_db).await?;
            (
                Action::TeamInvitationDecline,
                EventOwner::default(),
                payload,
            )
        }
    };

    audit_record(&state, Some(user.id), action, owner, &info, Some(payload)).await;

    Ok(ok_json("Invitation declined".to_string()))
}
