/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::endpoints::teams::grants::grant_workers;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json, role_names};
use crate::resource::Resource;
use crate::scope::{Admin, Member, TeamScope};
use axum::{Extension, Json};
pub use gradient_api_types::{
    RequestKind, TeamCacheGrant, TeamGrants, TeamProjectGrant, TeamRequestItem,
};
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::TransactionTrait;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

fn build_team_project_grant_api_item_from_grant_row_project_row_and_role_name(
    grant: &MTeamProject,
    project: &MProject,
    role: Option<String>,
) -> TeamProjectGrant {
    TeamProjectGrant {
        project: project.name.clone(),
        display_name: project.display_name.clone(),
        role,
        users: grant.includes_users,
        workers: grant.includes_workers,
    }
}

fn team_cache_grant_from_cache_row_and_role_name(
    cache: &MCache,
    role: Option<String>,
) -> TeamCacheGrant {
    TeamCacheGrant {
        cache: cache.name.clone(),
        display_name: cache.display_name.clone(),
        role,
    }
}

fn team_request_item_from_cache_access_request(
    request: &MTeamCacheRequest,
    cache: &MCache,
    role: Option<String>,
    requested_by: Option<String>,
) -> TeamRequestItem {
    TeamRequestItem {
        id: request.id.into_inner(),
        kind: RequestKind::Cache,
        target: cache.name.clone(),
        display_name: cache.display_name.clone(),
        role,
        users: true,
        workers: false,
        requested_by,
        created_at: request.created_at,
    }
}

fn team_request_item_from_project_access_request_row_project_role_and_user_name(
    request: &MTeamProjectRequest,
    project: &MProject,
    role: Option<String>,
    requested_by: Option<String>,
) -> TeamRequestItem {
    TeamRequestItem {
        id: request.id.into_inner(),
        kind: RequestKind::Project,
        target: project.name.clone(),
        display_name: project.display_name.clone(),
        role,
        users: request.includes_users,
        workers: request.includes_workers,
        requested_by,
        created_at: request.created_at,
    }
}

async fn cache_role_names(db: &WebDb, ids: Vec<RoleId>) -> WebResult<HashMap<RoleId, String>> {
    Ok(by_ids::<ECacheRole, _>(db, &ids)
        .await?
        .into_iter()
        .map(|(id, role)| (id, role.name))
        .collect())
}

#[utoipa::path(
    get,
    path = "/teams/{team}/grants",
    operation_id = "listTeamGrants",
    tag = "teams",
    summary = "List team grants",
    description = "Projects and caches that grant the team.",
    params(TeamPath),
    responses((status = OK, description = "OK", body = TeamGrants), WebError)
)]
pub async fn get_team_grants(
    TeamScope { team, db, .. }: TeamScope<Member>,
) -> WebResult<Json<BaseResponse<TeamGrants>>> {
    let project_grants =
        gradient_db::teams::grants::project_grants_of_team(&db.web_db, team.id).await?;
    let cache_grants =
        gradient_db::teams::grants::cache_grants_of_team(&db.web_db, team.id).await?;

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
            let role = g.role.and_then(|role| project_roles.get(&role).cloned());
            Some(
                build_team_project_grant_api_item_from_grant_row_project_row_and_role_name(
                    g, project, role,
                ),
            )
        })
        .collect();
    project_items.sort_by(|a, b| a.project.cmp(&b.project));

    let mut cache_items: Vec<TeamCacheGrant> = cache_grants
        .iter()
        .filter_map(|g| {
            let cache = caches.get(&g.cache)?;
            Some(team_cache_grant_from_cache_row_and_role_name(
                cache,
                cache_roles.get(&g.role).cloned(),
            ))
        })
        .collect();
    cache_items.sort_by(|a, b| a.cache.cmp(&b.cache));

    Ok(ok_json(TeamGrants {
        projects: project_items,
        caches: cache_items,
    }))
}

#[utoipa::path(
    get,
    path = "/teams/{team}/requests",
    operation_id = "listTeamRequests",
    tag = "teams",
    summary = "List grant requests",
    description = "Needs Admin in the team. Newest first.",
    params(TeamPath),
    responses((status = OK, description = "OK", body = Vec<TeamRequestItem>), WebError)
)]
pub async fn get_team_requests(
    TeamScope { team, db, .. }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<Vec<TeamRequestItem>>>> {
    let items = build_team_request_items_for_team_ids(&db.web_db, &[team.id])
        .await?
        .into_iter()
        .map(|(_, item)| item)
        .collect();
    Ok(ok_json(items))
}

pub(crate) async fn build_team_request_items_for_team_ids(
    db: &WebDb,
    teams: &[TeamId],
) -> WebResult<Vec<(TeamId, TeamRequestItem)>> {
    let mut items = Vec::new();
    for team in teams {
        items.extend(
            team_request_items(db, *team)
                .await?
                .into_iter()
                .map(|item| (*team, item)),
        );
    }
    Ok(items)
}

async fn team_request_items(db: &WebDb, team: TeamId) -> WebResult<Vec<TeamRequestItem>> {
    let project_requests = gradient_db::teams::requests::project_requests_of_team(db, team).await?;
    let cache_requests = gradient_db::teams::requests::cache_requests_of_team(db, team).await?;

    let projects = by_ids::<EProject, _>(
        db,
        &project_requests
            .iter()
            .map(|r| r.project)
            .collect::<Vec<_>>(),
    )
    .await?;
    let caches = by_ids::<ECache, _>(
        db,
        &cache_requests.iter().map(|r| r.cache).collect::<Vec<_>>(),
    )
    .await?;
    let project_roles =
        role_names(db, project_requests.iter().filter_map(|r| r.role).collect()).await?;
    let cache_roles = cache_role_names(db, cache_requests.iter().map(|r| r.role).collect()).await?;
    let requester_ids: Vec<UserId> = project_requests
        .iter()
        .filter_map(|r| r.requested_by)
        .chain(cache_requests.iter().filter_map(|r| r.requested_by))
        .collect();
    let requesters: HashMap<UserId, String> = by_ids::<EUser, _>(db, &requester_ids)
        .await?
        .into_iter()
        .map(|(id, u)| (id, u.username))
        .collect();
    let requester = |id: Option<UserId>| id.and_then(|id| requesters.get(&id).cloned());

    let mut items: Vec<TeamRequestItem> = project_requests
        .iter()
        .filter_map(|r| {
            let project = projects.get(&r.project)?;
            let role = r.role.and_then(|role| project_roles.get(&role).cloned());
            Some(
                team_request_item_from_project_access_request_row_project_role_and_user_name(
                    r,
                    project,
                    role,
                    requester(r.requested_by),
                ),
            )
        })
        .chain(cache_requests.iter().filter_map(|r| {
            let cache = caches.get(&r.cache)?;
            Some(team_request_item_from_cache_access_request(
                r,
                cache,
                cache_roles.get(&r.role).cloned(),
                requester(r.requested_by),
            ))
        }))
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.created_at));

    Ok(items)
}

#[utoipa::path(
    post,
    path = "/teams/{team}/requests/{request_id}",
    operation_id = "approveTeamRequest",
    tag = "teams",
    summary = "Approve grant request",
    description = "Needs Admin in the team. The grant becomes active.",
    params(TeamPath, ("request_id" = Uuid, Path)),
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_approve_team_request(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let request_id: Uuid = params.param("request_id")?;

    if let Some(request) = gradient_db::teams::requests::project_request_by_id(
        &db.web_db,
        team.id,
        TeamProjectRequestId::new(request_id),
    )
    .await?
    {
        let (project, workers) = (request.project, request.includes_workers);
        let tx = db.web_db.inner().begin().await?;
        gradient_db::teams::grants::grant_project(
            &tx,
            MTeamProject {
                id: TeamProjectId::now_v7(),
                team: team.id,
                project,
                role: request.role,
                includes_users: request.includes_users,
                includes_workers: workers,
                grant_source: TeamGrantSource::Manual,
                created_at: gradient_types::now(),
            },
        )
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;
        tx.commit().await?;

        if workers {
            grant_workers(&db.web_db, &scheduler, team.id, project).await?;
        }
        audit.write(Action::TeamRequestApprove, EventOwner::project(project), serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.to_string() }))
        .await;
        return Ok(ok_json("Request approved".to_string()));
    }

    let request = gradient_db::teams::requests::cache_request_by_id(
        &db.web_db,
        team.id,
        TeamCacheRequestId::new(request_id),
    )
    .await?
    .or_not_found(Resource::Request)?;
    let cache = request.cache;
    let tx = db.web_db.inner().begin().await?;
    gradient_db::teams::grants::grant_cache(
        &tx,
        MTeamCache {
            id: TeamCacheId::now_v7(),
            team: team.id,
            cache,
            role: request.role,
            grant_source: TeamGrantSource::Manual,
            created_at: gradient_types::now(),
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;
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

#[utoipa::path(
    delete,
    path = "/teams/{team}/requests/{request_id}",
    operation_id = "denyTeamRequest",
    tag = "teams",
    summary = "Deny grant request",
    description = "Needs Admin in the team.",
    params(TeamPath, ("request_id" = Uuid, Path)),
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_team_request(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let request_id: Uuid = params.param("request_id")?;

    let owner = if let Some(request) = gradient_db::teams::requests::project_request_by_id(
        &db.web_db,
        team.id,
        TeamProjectRequestId::new(request_id),
    )
    .await?
    {
        let project = request.project;
        gradient_db::teams::requests::remove_project_request(&db.web_db, request).await?;
        EventOwner::project(project)
    } else {
        let request = gradient_db::teams::requests::cache_request_by_id(
            &db.web_db,
            team.id,
            TeamCacheRequestId::new(request_id),
        )
        .await?
        .or_not_found(Resource::Request)?;
        let cache = request.cache;
        gradient_db::teams::requests::remove_cache_request(&db.web_db, request).await?;
        EventOwner::cache(cache)
    };

    audit.write(Action::TeamRequestDeny, owner, serde_json::json!({ "team_id": team.id.to_string(), "request_id": request_id.to_string() }))
    .await;
    Ok(ok_json("Request denied".to_string()))
}
