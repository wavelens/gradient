/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, events, record as audit_record};
use crate::error::{WebError, WebResult};
use crate::helpers::{ok_json, role_names};
use crate::invite_policy::{
    InviteDecision, InviteItem, InviteKind, evaluate_invite, merge_invites,
};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
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

    let inviter_ids: Vec<UserId> = project_rows
        .iter()
        .map(|r| r.invited_by)
        .chain(cache_rows.iter().map(|r| r.invited_by))
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

    let project_items = project_rows
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

    let cache_items = cache_rows
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

    Ok(ok_json(merge_invites(project_items, cache_items)))
}

enum Invitation {
    Project(MProjectInvitation),
    Cache(MCacheInvitation),
}

impl Invitation {
    fn invitee(&self) -> UserId {
        match self {
            Self::Project(i) => i.user,
            Self::Cache(i) => i.user,
        }
    }

    fn expires_at(&self) -> chrono::NaiveDateTime {
        match self {
            Self::Project(i) => i.expires_at,
            Self::Cache(i) => i.expires_at,
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

    ECacheInvitation::find()
        .filter(CCacheInvitation::Token.eq(token))
        .one(&state.web_db)
        .await?
        .map(Invitation::Cache)
        .ok_or_else(|| WebError::not_found("Invitation"))
}

/// Resolves the token, then enforces that the session belongs to the invitee.
/// A forwarded mail is therefore useless to anybody else.
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

    let (event, payload) = match invitation {
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

            (events::PROJECT_INVITATION_ACCEPT, payload)
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

            (events::CACHE_INVITATION_ACCEPT, payload)
        }
    };

    tx.commit().await?;
    audit_record(&state.web_db, Some(user.id), event, &info, Some(payload)).await;

    Ok(ok_json("Invitation accepted".to_string()))
}

pub async fn post_decline_invite(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Json(body): Json<InviteTokenRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let invitation = claim_invitation(&state, &user, &body.token).await?;

    let (event, payload) = match invitation {
        Invitation::Project(inv) => {
            let payload = serde_json::json!({ "project_id": inv.project.to_string() });
            inv.into_active_model().delete(&state.web_db).await?;
            (events::PROJECT_INVITATION_DECLINE, payload)
        }
        Invitation::Cache(inv) => {
            let payload = serde_json::json!({ "cache_id": inv.cache.to_string() });
            inv.into_active_model().delete(&state.web_db).await?;
            (events::CACHE_INVITATION_DECLINE, payload)
        }
    };

    audit_record(&state.web_db, Some(user.id), event, &info, Some(payload)).await;

    Ok(ok_json("Invitation declined".to_string()))
}
