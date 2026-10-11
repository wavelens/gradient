/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::endpoints::teams::grants::grant_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::resource::Resource;
use crate::scope::{Admin, Member, TeamScope};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
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

async fn cache_role_names(db: &WebDb, ids: Vec<RoleId>) -> WebResult<HashMap<RoleId, String>> {
    Ok(by_ids::<ECacheRole, _>(db, &ids)
        .await?
        .into_iter()
        .map(|(id, role)| (id, role.name))
        .collect())
}

pub async fn get_team_grants(
    TeamScope { team, db, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<TeamGrants>>> {
    let project_grants = ETeamProject::find()
        .filter(CTeamProject::Team.eq(team.id))
        .all(&db.web_db)
        .await?;
    let cache_grants = ETeamCache::find()
        .filter(CTeamCache::Team.eq(team.id))
        .all(&db.web_db)
        .await?;

    let projects = by_ids::<EProject, _>(
        &db.web_db,
        &project_grants.iter().map(|g| g.project).collect::<Vec<_>>(),
    )
    .await?;
    let caches = by_ids::<ECache, _>(
        &db.web_db,
        &cache_grants.iter().map(|g| g.cache).collect::<Vec<_>>(),
    )
    .await?;
    let project_roles = role_names(
        &db.web_db,
        project_grants.iter().filter_map(|g| g.role).collect(),
    )
    .await?;
    let cache_roles =
        cache_role_names(&db.web_db, cache_grants.iter().map(|g| g.role).collect()).await?;

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
    TeamScope { team, db, .. }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<Vec<TeamRequestItem>>>> {
    let project_requests = ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Team.eq(team.id))
        .all(&db.web_db)
        .await?;
    let cache_requests = ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Team.eq(team.id))
        .all(&db.web_db)
        .await?;

    let projects = by_ids::<EProject, _>(
        &db.web_db,
        &project_requests
            .iter()
            .map(|r| r.project)
            .collect::<Vec<_>>(),
    )
    .await?;
    let caches = by_ids::<ECache, _>(
        &db.web_db,
        &cache_requests.iter().map(|r| r.cache).collect::<Vec<_>>(),
    )
    .await?;
    let project_roles = role_names(
        &db.web_db,
        project_requests.iter().filter_map(|r| r.role).collect(),
    )
    .await?;
    let cache_roles =
        cache_role_names(&db.web_db, cache_requests.iter().map(|r| r.role).collect()).await?;
    let requester_ids: Vec<UserId> = project_requests
        .iter()
        .filter_map(|r| r.requested_by)
        .chain(cache_requests.iter().filter_map(|r| r.requested_by))
        .collect();
    let requesters: HashMap<UserId, String> = by_ids::<EUser, _>(&db.web_db, &requester_ids)
        .await?
        .into_iter()
        .map(|(id, u)| (id, u.username))
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
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let request_id: Uuid = params.param("request_id")?;

    if let Some(request) = ETeamProjectRequest::find_by_id(TeamProjectRequestId::new(request_id))
        .filter(CTeamProjectRequest::Team.eq(team.id))
        .one(&db.web_db)
        .await?
    {
        let (project, workers) = (request.project, request.includes_workers);
        let tx = db.web_db.inner().begin().await?;
        MTeamProject {
            id: TeamProjectId::now_v7(),
            team: team.id,
            project,
            role: request.role,
            includes_users: request.includes_users,
            includes_workers: workers,
            managed: false,
            created_at: gradient_types::now(),
        }
        .into_active_model()
        .insert(&tx)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;
        request.into_active_model().delete(&tx).await?;
        tx.commit().await?;

        if workers {
            grant_workers(&db.web_db, &scheduler, team.id, project).await?;
        }
        audit.write(Action::TeamRequestApprove, EventOwner::project(project), serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.to_string() }))
        .await;
        return Ok(ok_json("Request approved".to_string()));
    }

    let request = ETeamCacheRequest::find_by_id(TeamCacheRequestId::new(request_id))
        .filter(CTeamCacheRequest::Team.eq(team.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Request)?;
    let cache = request.cache;
    let tx = db.web_db.inner().begin().await?;
    MTeamCache {
        id: TeamCacheId::now_v7(),
        team: team.id,
        cache,
        role: request.role,
        managed: false,
        created_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;
    request.into_active_model().delete(&tx).await?;
    tx.commit().await?;

    audit
        .write(
            Action::TeamRequestApprove,
            EventOwner::cache(cache),
            serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.to_string() }),
        )
        .await;
    Ok(ok_json("Request approved".to_string()))
}

pub async fn delete_team_request(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let request_id: Uuid = params.param("request_id")?;

    let owner = if let Some(request) =
        ETeamProjectRequest::find_by_id(TeamProjectRequestId::new(request_id))
            .filter(CTeamProjectRequest::Team.eq(team.id))
            .one(&db.web_db)
            .await?
    {
        let project = request.project;
        request.into_active_model().delete(&db.web_db).await?;
        EventOwner::project(project)
    } else {
        let request = ETeamCacheRequest::find_by_id(TeamCacheRequestId::new(request_id))
            .filter(CTeamCacheRequest::Team.eq(team.id))
            .one(&db.web_db)
            .await?
            .or_not_found(Resource::Request)?;
        let cache = request.cache;
        request.into_active_model().delete(&db.web_db).await?;
        EventOwner::cache(cache)
    };

    audit.write(Action::TeamRequestDeny, owner, serde_json::json!({ "team_id": team.id.to_string(), "request_id": request_id.to_string() }))
    .await;
    Ok(ok_json("Request denied".to_string()))
}
