/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{TeamAccess, load_team};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::endpoints::teams::grants::grant_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, TransactionTrait,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct TeamProjectGrant {
    pub project: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
}

#[derive(Serialize)]
pub struct TeamCacheGrant {
    pub cache: String,
    pub display_name: String,
    pub role: Option<String>,
}

#[derive(Serialize)]
pub struct TeamGrants {
    pub projects: Vec<TeamProjectGrant>,
    pub caches: Vec<TeamCacheGrant>,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RequestKind {
    Project,
    Cache,
}

#[derive(Serialize)]
pub struct TeamRequestItem {
    pub id: Uuid,
    pub kind: RequestKind,
    pub target: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
    pub requested_by: Option<String>,
    pub created_at: NaiveDateTime,
}

async fn cache_role_names(
    state: &Arc<ServerState>,
    ids: Vec<RoleId>,
) -> WebResult<HashMap<RoleId, String>> {
    Ok(ECacheRole::find()
        .filter(CCacheRole::Id.is_in(ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|role| (role.id, role.name))
        .collect())
}

async fn projects_by_id(
    state: &Arc<ServerState>,
    ids: Vec<ProjectId>,
) -> WebResult<HashMap<ProjectId, MProject>> {
    Ok(EProject::find()
        .filter(CProject::Id.is_in(ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|project| (project.id, project))
        .collect())
}

async fn caches_by_id(
    state: &Arc<ServerState>,
    ids: Vec<CacheId>,
) -> WebResult<HashMap<CacheId, MCache>> {
    Ok(ECache::find()
        .filter(CCache::Id.is_in(ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|cache| (cache.id, cache))
        .collect())
}

pub async fn get_team_grants(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
) -> WebResult<Json<BaseResponse<TeamGrants>>> {
    let (team, _) = load_team(&state, &user, api_key.as_ref(), team, TeamAccess::Member).await?;

    let project_grants = ETeamProject::find()
        .filter(CTeamProject::Team.eq(team.id))
        .all(&state.web_db)
        .await?;
    let cache_grants = ETeamCache::find()
        .filter(CTeamCache::Team.eq(team.id))
        .all(&state.web_db)
        .await?;

    let projects =
        projects_by_id(&state, project_grants.iter().map(|g| g.project).collect()).await?;
    let caches = caches_by_id(&state, cache_grants.iter().map(|g| g.cache).collect()).await?;
    let project_roles = role_names(
        &state.web_db,
        project_grants.iter().filter_map(|g| g.role).collect(),
    )
    .await?;
    let cache_roles =
        cache_role_names(&state, cache_grants.iter().map(|g| g.role).collect()).await?;

    let mut project_items: Vec<TeamProjectGrant> = project_grants
        .iter()
        .filter_map(|g| {
            let project = projects.get(&g.project)?;
            Some(TeamProjectGrant {
                project: project.name.clone(),
                display_name: project.display_name.clone(),
                role: g.role.and_then(|role| project_roles.get(&role).cloned()),
                users: g.includes_users,
                workers: g.includes_workers,
            })
        })
        .collect();
    project_items.sort_by(|a, b| a.project.cmp(&b.project));

    let mut cache_items: Vec<TeamCacheGrant> = cache_grants
        .iter()
        .filter_map(|g| {
            let cache = caches.get(&g.cache)?;
            Some(TeamCacheGrant {
                cache: cache.name.clone(),
                display_name: cache.display_name.clone(),
                role: cache_roles.get(&g.role).cloned(),
            })
        })
        .collect();
    cache_items.sort_by(|a, b| a.cache.cmp(&b.cache));

    Ok(ok_json(TeamGrants {
        projects: project_items,
        caches: cache_items,
    }))
}

pub async fn get_team_requests(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(team): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<TeamRequestItem>>>> {
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

    let project_requests = ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Team.eq(team.id))
        .all(&state.web_db)
        .await?;
    let cache_requests = ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Team.eq(team.id))
        .all(&state.web_db)
        .await?;

    let projects =
        projects_by_id(&state, project_requests.iter().map(|r| r.project).collect()).await?;
    let caches = caches_by_id(&state, cache_requests.iter().map(|r| r.cache).collect()).await?;
    let project_roles = role_names(
        &state.web_db,
        project_requests.iter().filter_map(|r| r.role).collect(),
    )
    .await?;
    let cache_roles =
        cache_role_names(&state, cache_requests.iter().map(|r| r.role).collect()).await?;
    let requester_ids: Vec<UserId> = project_requests
        .iter()
        .filter_map(|r| r.requested_by)
        .chain(cache_requests.iter().filter_map(|r| r.requested_by))
        .collect();
    let requesters: HashMap<UserId, String> = EUser::find()
        .filter(CUser::Id.is_in(requester_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|u| (u.id, u.username))
        .collect();
    let requester = |id: Option<UserId>| id.and_then(|id| requesters.get(&id).cloned());

    let mut items: Vec<TeamRequestItem> = project_requests
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            Some(TeamRequestItem {
                id: r.id.into_inner(),
                kind: RequestKind::Project,
                target: project.name.clone(),
                display_name: project.display_name.clone(),
                role: r.role.and_then(|role| project_roles.get(&role).cloned()),
                users: r.includes_users,
                workers: r.includes_workers,
                requested_by: requester(r.requested_by),
                created_at: r.created_at,
            })
        })
        .chain(cache_requests.iter().filter_map(|r| {
            let cache = caches.get(&r.cache)?;
            Some(TeamRequestItem {
                id: r.id.into_inner(),
                kind: RequestKind::Cache,
                target: cache.name.clone(),
                display_name: cache.display_name.clone(),
                role: cache_roles.get(&r.role).cloned(),
                users: true,
                workers: false,
                requested_by: requester(r.requested_by),
                created_at: r.created_at,
            })
        }))
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.created_at));

    Ok(ok_json(items))
}

pub async fn post_approve_team_request(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((team, request_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<String>>> {
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

    if let Some(request) = ETeamProjectRequest::find_by_id(TeamProjectRequestId::new(request_id))
        .filter(CTeamProjectRequest::Team.eq(team.id))
        .one(&state.web_db)
        .await?
    {
        let (project, workers) = (request.project, request.includes_workers);
        let tx = state.web_db.inner().begin().await?;
        MTeamProject {
            id: TeamProjectId::now_v7(),
            team: team.id,
            project,
            role: request.role,
            includes_users: request.includes_users,
            includes_workers: workers,
            created_at: gradient_types::now(),
        }
        .into_active_model()
        .insert(&tx)
        .await
        .map_err(|e| WebError::from_db_err(e, "Team grant"))?;
        request.into_active_model().delete(&tx).await?;
        tx.commit().await?;

        if workers {
            grant_workers(&state, &scheduler, team.id, project).await?;
        }
        audit_record(
            &state,
            Some(user.id),
            Action::TeamRequestApprove,
            EventOwner { project: Some(project), ..Default::default() },
            &info,
            Some(serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.to_string() })),
        )
        .await;
        return Ok(ok_json("Request approved".to_string()));
    }

    let request = ETeamCacheRequest::find_by_id(TeamCacheRequestId::new(request_id))
        .filter(CTeamCacheRequest::Team.eq(team.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Request")?;
    let cache = request.cache;
    let tx = state.web_db.inner().begin().await?;
    MTeamCache {
        id: TeamCacheId::now_v7(),
        team: team.id,
        cache,
        role: request.role,
        created_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, "Team grant"))?;
    request.into_active_model().delete(&tx).await?;
    tx.commit().await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamRequestApprove,
        EventOwner {
            cache: Some(cache),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.to_string() })),
    )
    .await;
    Ok(ok_json("Request approved".to_string()))
}

pub async fn delete_team_request(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((team, request_id)): Path<(String, Uuid)>,
) -> WebResult<Json<BaseResponse<String>>> {
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

    let owner = if let Some(request) =
        ETeamProjectRequest::find_by_id(TeamProjectRequestId::new(request_id))
            .filter(CTeamProjectRequest::Team.eq(team.id))
            .one(&state.web_db)
            .await?
    {
        let project = request.project;
        request.into_active_model().delete(&state.web_db).await?;
        EventOwner {
            project: Some(project),
            ..Default::default()
        }
    } else {
        let request = ETeamCacheRequest::find_by_id(TeamCacheRequestId::new(request_id))
            .filter(CTeamCacheRequest::Team.eq(team.id))
            .one(&state.web_db)
            .await?
            .or_not_found("Request")?;
        let cache = request.cache;
        request.into_active_model().delete(&state.web_db).await?;
        EventOwner {
            cache: Some(cache),
            ..Default::default()
        }
    };

    audit_record(
        &state,
        Some(user.id),
        Action::TeamRequestDeny,
        owner,
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "request_id": request_id.to_string() })),
    )
    .await;
    Ok(ok_json("Request denied".to_string()))
}
