/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::endpoints::teams::members::role_label;
use crate::error::{WebError, WebResult};
use crate::helpers::{ok_json, role_names};
use crate::invite_policy::{
    InviteDecision, InviteItem, InviteKind, evaluate_invite, merge_invites,
};
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
enum InviteRefusal {
    #[error("Invitation has expired")]
    ExpiredInvitation,
}

impl Refusal for InviteRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::ExpiredInvitation => RefusalStatus::Gone,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InviteTokenRequest {
    pub token: String,
}

pub async fn get_user_invites(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<InviteItem>>>> {
    let project_rows = gradient_db::invitations::of_user::<EProjectInvitation, _>(
        &state.web_db,
        CProjectInvitation::User,
        user.id,
    )
    .await?;
    let cache_rows = gradient_db::invitations::of_user::<ECacheInvitation, _>(
        &state.web_db,
        CCacheInvitation::User,
        user.id,
    )
    .await?;
    let team_rows = gradient_db::invitations::of_user::<ETeamInvitation, _>(
        &state.web_db,
        CTeamInvitation::User,
        user.id,
    )
    .await?;

    let inviter_ids: Vec<UserId> = project_rows
        .iter()
        .map(|r| r.invited_by)
        .chain(cache_rows.iter().map(|r| r.invited_by))
        .chain(team_rows.iter().map(|r| r.invited_by))
        .collect();
    let inviters: HashMap<UserId, String> = by_ids::<EUser, _>(&state.web_db, &inviter_ids)
        .await?
        .into_iter()
        .map(|(id, u)| (id, u.username))
        .collect();

    let projects: HashMap<ProjectId, MProject> = by_ids::<EProject, _>(
        &state.web_db,
        &project_rows.iter().map(|r| r.project).collect::<Vec<_>>(),
    )
    .await?;
    let caches: HashMap<CacheId, MCache> = by_ids::<ECache, _>(
        &state.web_db,
        &cache_rows.iter().map(|r| r.cache).collect::<Vec<_>>(),
    )
    .await?;
    let teams: HashMap<TeamId, MTeam> = by_ids::<ETeam, _>(
        &state.web_db,
        &team_rows.iter().map(|r| r.team).collect::<Vec<_>>(),
    )
    .await?;

    let project_roles = role_names(
        &state.web_db,
        project_rows.iter().map(|r| r.role).collect::<Vec<_>>(),
    )
    .await?;
    let cache_roles: HashMap<RoleId, String> = by_ids::<ECacheRole, _>(
        &state.web_db,
        &cache_rows.iter().map(|r| r.role).collect::<Vec<_>>(),
    )
    .await?
    .into_iter()
    .map(|(id, r)| (id, r.name))
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

async fn find_invitation(db: &WebDb, token: &str) -> WebResult<Invitation> {
    if let Some(row) = gradient_db::invitations::by_token::<EProjectInvitation, _>(
        db,
        CProjectInvitation::Token,
        token,
    )
    .await?
    {
        return Ok(Invitation::Project(row));
    }

    if let Some(row) = gradient_db::invitations::by_token::<ECacheInvitation, _>(
        db,
        CCacheInvitation::Token,
        token,
    )
    .await?
    {
        return Ok(Invitation::Cache(row));
    }

    gradient_db::invitations::by_token::<ETeamInvitation, _>(db, CTeamInvitation::Token, token)
        .await?
        .map(Invitation::Team)
        .ok_or_else(|| WebError::not_found(Resource::Invitation))
}

/// The session must belong to the invitee. A forwarded mail is useless to anybody else.
async fn claim_invitation(db: &WebDb, user: &MUser, token: &str) -> WebResult<Invitation> {
    let invitation = find_invitation(db, token).await?;

    match evaluate_invite(
        invitation.invitee(),
        user.id,
        invitation.expires_at(),
        gradient_types::now(),
    ) {
        InviteDecision::Redeem => Ok(invitation),
        InviteDecision::NotInvitee => Err(WebError::not_found(Resource::Invitation)),
        InviteDecision::Expired => {
            match invitation {
                Invitation::Project(i) => {
                    gradient_db::rows::delete::<EProjectInvitation, _>(db, i).await?;
                }
                Invitation::Cache(i) => {
                    gradient_db::rows::delete::<ECacheInvitation, _>(db, i).await?;
                }
                Invitation::Team(i) => {
                    gradient_db::rows::delete::<ETeamInvitation, _>(db, i).await?;
                }
            }

            Err(InviteRefusal::ExpiredInvitation.into())
        }
    }
}

pub async fn post_accept_invite(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Json(body): Json<InviteTokenRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let invitation = claim_invitation(&state.web_db, &user, &body.token).await?;
    let tx = state.web_db.inner().begin().await?;

    let (action, owner, payload) = match invitation {
        Invitation::Project(inv) => {
            let already = gradient_db::memberships::by_owner_and_user::<EProjectUser, _>(
                &tx,
                CProjectUser::Project,
                inv.project,
                CProjectUser::User,
                inv.user,
            )
            .await?
            .is_some();

            let payload = serde_json::json!({
                "project_id": inv.project.to_string(),
                "role_id": inv.role.to_string(),
            });
            let project = inv.project;
            let role = inv.role;
            let member = inv.user;
            gradient_db::rows::delete::<EProjectInvitation, _>(&tx, inv).await?;

            if !already {
                gradient_db::rows::insert::<EProjectUser, _>(
                    &tx,
                    MProjectUser {
                        id: ProjectUserId::now_v7(),
                        project,
                        user: member,
                        role,
                    },
                )
                .await?;
            }

            (
                Action::ProjectInvitationAccept,
                EventOwner::project(project),
                payload,
            )
        }
        Invitation::Cache(inv) => {
            let already = gradient_db::memberships::by_owner_and_user::<ECacheUser, _>(
                &tx,
                CCacheUser::Cache,
                inv.cache,
                CCacheUser::User,
                inv.user,
            )
            .await?
            .is_some();

            let payload = serde_json::json!({
                "cache_id": inv.cache.to_string(),
                "role_id": inv.role.to_string(),
            });
            let cache = inv.cache;
            let role = inv.role;
            let member = inv.user;
            gradient_db::rows::delete::<ECacheInvitation, _>(&tx, inv).await?;

            if !already {
                gradient_db::rows::insert::<ECacheUser, _>(
                    &tx,
                    MCacheUser {
                        id: CacheUserId::now_v7(),
                        cache,
                        user: member,
                        role,
                    },
                )
                .await?;
            }

            (
                Action::CacheInvitationAccept,
                EventOwner::cache(cache),
                payload,
            )
        }
        Invitation::Team(inv) => {
            let already = gradient_db::teams::members::membership(&tx, inv.team, inv.user)
                .await?
                .is_some();

            let payload = serde_json::json!({ "team_id": inv.team.to_string() });
            let (team, role, member) = (inv.team, inv.role, inv.user);
            gradient_db::rows::delete::<ETeamInvitation, _>(&tx, inv).await?;

            if !already {
                gradient_db::teams::members::add_member(
                    &tx,
                    MTeamUser {
                        id: TeamUserId::now_v7(),
                        team,
                        user: member,
                        role,
                        source: TeamMemberSource::Api,
                    },
                )
                .await?;
            }

            (Action::TeamInvitationAccept, EventOwner::default(), payload)
        }
    };

    tx.commit().await?;
    audit.write(action, owner, payload).await;

    Ok(ok_json("Invitation accepted".to_string()))
}

pub async fn post_decline_invite(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Json(body): Json<InviteTokenRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let invitation = claim_invitation(&state.web_db, &user, &body.token).await?;

    let (action, owner, payload) = match invitation {
        Invitation::Project(inv) => {
            let owner = EventOwner::project(inv.project);
            let payload = serde_json::json!({ "project_id": inv.project.to_string() });
            gradient_db::rows::delete::<EProjectInvitation, _>(&state.web_db, inv).await?;
            (Action::ProjectInvitationDecline, owner, payload)
        }
        Invitation::Cache(inv) => {
            let owner = EventOwner::cache(inv.cache);
            let payload = serde_json::json!({ "cache_id": inv.cache.to_string() });
            gradient_db::rows::delete::<ECacheInvitation, _>(&state.web_db, inv).await?;
            (Action::CacheInvitationDecline, owner, payload)
        }
        Invitation::Team(inv) => {
            let payload = serde_json::json!({ "team_id": inv.team.to_string() });
            gradient_db::rows::delete::<ETeamInvitation, _>(&state.web_db, inv).await?;
            (
                Action::TeamInvitationDecline,
                EventOwner::default(),
                payload,
            )
        }
    };

    audit.write(action, owner, payload).await;

    Ok(ok_json("Invitation declined".to_string()))
}
