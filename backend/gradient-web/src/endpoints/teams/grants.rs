/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, has_cache_permission, has_permission, load_cache,
    load_project,
};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::teams::workers::{reauth_team_workers, withdraw_team_workers};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{CachePermission, Permission};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::lookup::{get_any_cache_by_name, get_any_project_by_name};
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Serialize)]
pub struct TeamGrantEntry {
    pub team: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
    pub pending: bool,
}

#[derive(Deserialize)]
pub struct GrantProjectRequest {
    pub team: String,
    pub role: Option<String>,
    #[serde(default)]
    pub users: bool,
    #[serde(default)]
    pub workers: bool,
}

#[derive(Deserialize)]
pub struct PatchProjectGrantRequest {
    pub role: Option<String>,
    pub users: Option<bool>,
    pub workers: Option<bool>,
}

#[derive(Deserialize)]
pub struct GrantCacheRequest {
    pub team: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct PatchCacheGrantRequest {
    pub role: String,
}

async fn find_team(state: &Arc<ServerState>, name: &str) -> WebResult<MTeam> {
    ETeam::find()
        .filter(CTeam::Name.eq(name))
        .one(&state.web_db)
        .await?
        .or_not_found("Team")
}

async fn removes_as_team_admin(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    team: TeamId,
) -> WebResult<bool> {
    let pinned = api_key
        .as_ref()
        .is_some_and(|k| k.project.is_some() || k.cache_pin.is_some());
    Ok(!pinned && is_team_admin(state, user, team).await?)
}

async fn is_team_admin(state: &Arc<ServerState>, user: &MUser, team: TeamId) -> WebResult<bool> {
    Ok(user.superuser
        || gradient_db::teams::team_role_of(&state.web_db, team, user.id).await?
            == Some(TeamRole::Admin))
}

async fn project_role(
    state: &Arc<ServerState>,
    project: ProjectId,
    name: &str,
) -> WebResult<RoleId> {
    Ok(ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(name)).add(
                Condition::any()
                    .add(CRole::Project.eq(project))
                    .add(CRole::Project.is_null()),
            ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?
        .id)
}

async fn cache_role(state: &Arc<ServerState>, cache: CacheId, name: &str) -> WebResult<RoleId> {
    Ok(ECacheRole::find()
        .filter(
            Condition::all().add(CCacheRole::Name.eq(name)).add(
                Condition::any()
                    .add(CCacheRole::Cache.eq(cache))
                    .add(CCacheRole::Cache.is_null()),
            ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found("Role")?
        .id)
}

async fn project_grant(
    state: &Arc<ServerState>,
    team: TeamId,
    project: ProjectId,
) -> WebResult<Option<MTeamProject>> {
    Ok(ETeamProject::find()
        .filter(CTeamProject::Team.eq(team))
        .filter(CTeamProject::Project.eq(project))
        .one(&state.web_db)
        .await?)
}

pub(crate) async fn grant_workers(
    state: &Arc<ServerState>,
    scheduler: &Scheduler,
    team: TeamId,
    project: ProjectId,
) -> WebResult<()> {
    let workers = gradient_db::teams::team_worker_ids(&state.web_db, team).await?;
    reauth_team_workers(scheduler, &workers).await;
    if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project).await {
        tracing::warn!(error = %e, project_id = %project, "failed to unpark evaluations after a team grant");
    }
    Ok(())
}

async fn take_workers(
    state: &Arc<ServerState>,
    scheduler: &Scheduler,
    team: TeamId,
    project: ProjectId,
) -> WebResult<()> {
    let workers = gradient_db::teams::team_worker_ids(&state.web_db, team).await?;
    withdraw_team_workers(scheduler, &workers, &HashSet::from([project])).await;
    Ok(())
}

fn project_role_name(roles: &HashMap<RoleId, String>, role: Option<RoleId>) -> Option<String> {
    role.and_then(|id| roles.get(&id).cloned())
}

pub async fn get_project_teams(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<TeamGrantEntry>>>> {
    let project = load_project(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable { label: "Project" },
    )
    .await?;

    let grants = ETeamProject::find()
        .filter(CTeamProject::Project.eq(project.id))
        .all(&state.web_db)
        .await?;
    let requests = ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Project.eq(project.id))
        .all(&state.web_db)
        .await?;

    let team_ids: Vec<TeamId> = grants
        .iter()
        .map(|g| g.team)
        .chain(requests.iter().map(|r| r.team))
        .collect();
    let teams: HashMap<TeamId, MTeam> = ETeam::find()
        .filter(CTeam::Id.is_in(team_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|team| (team.id, team))
        .collect();
    let role_ids: Vec<RoleId> = grants
        .iter()
        .filter_map(|g| g.role)
        .chain(requests.iter().filter_map(|r| r.role))
        .collect();
    let roles = crate::helpers::role_names(&state.web_db, role_ids).await?;

    let entry = |team: TeamId, role, users, workers, pending| {
        teams.get(&team).map(|team| TeamGrantEntry {
            team: team.name.clone(),
            display_name: team.display_name.clone(),
            role: project_role_name(&roles, role),
            users,
            workers,
            pending,
        })
    };
    let items = grants
        .iter()
        .filter_map(|g| entry(g.team, g.role, g.includes_users, g.includes_workers, false))
        .chain(
            requests
                .iter()
                .filter_map(|r| entry(r.team, r.role, r.includes_users, r.includes_workers, true)),
        )
        .collect();

    Ok(ok_json(items))
}

pub async fn post_project_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(project): Path<String>,
    Json(body): Json<GrantProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !body.users && !body.workers {
        return Err(WebError::bad_request(
            "A grant includes users, workers or both.",
        ));
    }
    let permission = if body.users {
        Permission::ManageMembers
    } else {
        Permission::ManageWorkers
    };
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission,
            reject_managed: true,
        },
    )
    .await?;
    if body.users
        && body.workers
        && !has_permission(
            &state,
            user.id,
            project.id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?
    {
        return Err(WebError::forbidden(
            "You do not have permission to perform this action.",
        ));
    }

    let team = find_team(&state, &body.team).await?;
    let role = match (body.users, body.role.as_deref()) {
        (true, Some(name)) => Some(project_role(&state, project.id, name).await?),
        (true, None) => {
            return Err(WebError::bad_request(
                "A role is required when the grant includes users.",
            ));
        }
        (false, _) => None,
    };
    if project_grant(&state, team.id, project.id).await?.is_some() {
        return Err(WebError::already_exists("Team grant"));
    }

    let payload =
        serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() });
    let owner = EventOwner {
        project: Some(project.id),
        ..Default::default()
    };

    if !is_team_admin(&state, &user, team.id).await? {
        MTeamProjectRequest {
            id: TeamProjectRequestId::now_v7(),
            team: team.id,
            project: project.id,
            role,
            includes_users: body.users,
            includes_workers: body.workers,
            requested_by: Some(user.id),
            created_at: gradient_types::now(),
        }
        .into_active_model()
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Team request"))?;
        audit_record(
            &state,
            Some(user.id),
            Action::TeamRequestCreate,
            owner,
            &info,
            Some(payload),
        )
        .await;
        return Ok(ok_json("Request sent".to_string()));
    }

    MTeamProject {
        id: TeamProjectId::now_v7(),
        team: team.id,
        project: project.id,
        role,
        includes_users: body.users,
        includes_workers: body.workers,
        managed: false,
        created_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;
    if body.workers {
        grant_workers(&state, &scheduler, team.id, project.id).await?;
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantCreate,
        owner,
        &info,
        Some(payload),
    )
    .await;
    Ok(ok_json("Team granted".to_string()))
}

pub async fn patch_project_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((project, team)): Path<(String, String)>,
    Json(body): Json<PatchProjectGrantRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let permission = if body.workers.is_some() && body.users.is_none() && body.role.is_none() {
        Permission::ManageWorkers
    } else {
        Permission::ManageMembers
    };
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission,
            reject_managed: true,
        },
    )
    .await?;
    let team = find_team(&state, &team).await?;
    let grant = project_grant(&state, team.id, project.id)
        .await?
        .or_not_found("Team grant")?;

    let users = body.users.unwrap_or(grant.includes_users);
    let workers = body.workers.unwrap_or(grant.includes_workers);
    if !users && !workers {
        return Err(WebError::bad_request(
            "A grant includes users, workers or both. Remove the grant instead.",
        ));
    }
    if workers && !grant.includes_workers && !is_team_admin(&state, &user, team.id).await? {
        return Err(WebError::forbidden(
            "Turning on a team's workers needs Admin in the team.",
        ));
    }
    let role = match (users, body.role.as_deref()) {
        (true, Some(name)) => Some(project_role(&state, project.id, name).await?),
        (true, None) => grant
            .role
            .ok_or_else(|| {
                WebError::bad_request("A role is required when the grant includes users.")
            })
            .map(Some)?,
        (false, _) => None,
    };

    let had_workers = grant.includes_workers;
    let mut active: ATeamProject = grant.into();
    active.includes_users = Set(users);
    active.includes_workers = Set(workers);
    active.role = Set(role);
    active.update(&state.web_db).await?;

    match (had_workers, workers) {
        (false, true) => grant_workers(&state, &scheduler, team.id, project.id).await?,
        (true, false) => take_workers(&state, &scheduler, team.id, project.id).await?,
        _ => {}
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantUpdate,
        EventOwner { project: Some(project.id), ..Default::default() },
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() })),
    )
    .await;
    Ok(ok_json("Team grant updated".to_string()))
}

pub async fn delete_project_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((project, team)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = get_any_project_by_name(&state.db(), project)
        .await?
        .or_not_found("Project")?;
    let team = find_team(&state, &team).await?;
    let grant = project_grant(&state, team.id, project.id).await?;
    let request = match grant {
        Some(_) => None,
        None => Some(
            ETeamProjectRequest::find()
                .filter(CTeamProjectRequest::Team.eq(team.id))
                .filter(CTeamProjectRequest::Project.eq(project.id))
                .one(&state.web_db)
                .await?
                .or_not_found("Team grant")?,
        ),
    };

    let allowed = has_permission(
        &state,
        user.id,
        project.id,
        Permission::ManageMembers,
        api_key.as_ref(),
    )
    .await?
        || removes_as_team_admin(&state, &user, &api_key, team.id).await?;
    if !allowed {
        return Err(WebError::not_found("Team grant"));
    }
    if project.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.",
        ));
    }

    let Some(grant) = grant else {
        if let Some(request) = request {
            request.into_active_model().delete(&state.web_db).await?;
        }
        return Ok(ok_json("Request withdrawn".to_string()));
    };
    let had_workers = grant.includes_workers;
    grant.into_active_model().delete(&state.web_db).await?;
    if had_workers {
        take_workers(&state, &scheduler, team.id, project.id).await?;
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantRemove,
        EventOwner { project: Some(project.id), ..Default::default() },
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() })),
    )
    .await;
    Ok(ok_json("Team grant removed".to_string()))
}

fn cache_role_name(roles: &HashMap<RoleId, String>, role: RoleId) -> Option<String> {
    roles.get(&role).cloned()
}

async fn cache_grant(
    state: &Arc<ServerState>,
    team: TeamId,
    cache: CacheId,
) -> WebResult<Option<MTeamCache>> {
    Ok(ETeamCache::find()
        .filter(CTeamCache::Team.eq(team))
        .filter(CTeamCache::Cache.eq(cache))
        .one(&state.web_db)
        .await?)
}

pub async fn get_cache_teams(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<TeamGrantEntry>>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ViewCache,
            reject_managed: false,
        },
    )
    .await?;

    let grants = ETeamCache::find()
        .filter(CTeamCache::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?;
    let requests = ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Cache.eq(cache.id))
        .all(&state.web_db)
        .await?;

    let team_ids: Vec<TeamId> = grants
        .iter()
        .map(|g| g.team)
        .chain(requests.iter().map(|r| r.team))
        .collect();
    let teams: HashMap<TeamId, MTeam> = ETeam::find()
        .filter(CTeam::Id.is_in(team_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|team| (team.id, team))
        .collect();
    let role_ids: Vec<RoleId> = grants
        .iter()
        .map(|g| g.role)
        .chain(requests.iter().map(|r| r.role))
        .collect();
    let roles: HashMap<RoleId, String> = ECacheRole::find()
        .filter(CCacheRole::Id.is_in(role_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|role| (role.id, role.name))
        .collect();

    let entry = |team: TeamId, role, pending| {
        teams.get(&team).map(|team| TeamGrantEntry {
            team: team.name.clone(),
            display_name: team.display_name.clone(),
            role: cache_role_name(&roles, role),
            users: true,
            workers: false,
            pending,
        })
    };
    let items = grants
        .iter()
        .filter_map(|g| entry(g.team, g.role, false))
        .chain(requests.iter().filter_map(|r| entry(r.team, r.role, true)))
        .collect();

    Ok(ok_json(items))
}

pub async fn post_cache_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
    Json(body): Json<GrantCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: true,
        },
    )
    .await?;
    let team = find_team(&state, &body.team).await?;
    let role = cache_role(&state, cache.id, &body.role).await?;
    if cache_grant(&state, team.id, cache.id).await?.is_some() {
        return Err(WebError::already_exists("Team grant"));
    }

    let payload =
        serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() });
    let owner = EventOwner {
        cache: Some(cache.id),
        ..Default::default()
    };

    if !is_team_admin(&state, &user, team.id).await? {
        MTeamCacheRequest {
            id: TeamCacheRequestId::now_v7(),
            team: team.id,
            cache: cache.id,
            role,
            requested_by: Some(user.id),
            created_at: gradient_types::now(),
        }
        .into_active_model()
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Team request"))?;
        audit_record(
            &state,
            Some(user.id),
            Action::TeamRequestCreate,
            owner,
            &info,
            Some(payload),
        )
        .await;
        return Ok(ok_json("Request sent".to_string()));
    }

    MTeamCache {
        id: TeamCacheId::now_v7(),
        team: team.id,
        cache: cache.id,
        role,
        managed: false,
        created_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantCreate,
        owner,
        &info,
        Some(payload),
    )
    .await;
    Ok(ok_json("Team granted".to_string()))
}

pub async fn patch_cache_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, team)): Path<(String, String)>,
    Json(body): Json<PatchCacheGrantRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheMembers,
            reject_managed: true,
        },
    )
    .await?;
    let team = find_team(&state, &team).await?;
    let grant = cache_grant(&state, team.id, cache.id)
        .await?
        .or_not_found("Team grant")?;
    let role = cache_role(&state, cache.id, &body.role).await?;

    let mut active: ATeamCache = grant.into();
    active.role = Set(role);
    active.update(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantUpdate,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(
            serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() }),
        ),
    )
    .await;
    Ok(ok_json("Team grant updated".to_string()))
}

pub async fn delete_cache_team(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, team)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_any_cache_by_name(&state.db(), cache)
        .await?
        .or_not_found("Cache")?;
    let team = find_team(&state, &team).await?;
    let grant = cache_grant(&state, team.id, cache.id).await?;
    let request = match grant {
        Some(_) => None,
        None => Some(
            ETeamCacheRequest::find()
                .filter(CTeamCacheRequest::Team.eq(team.id))
                .filter(CTeamCacheRequest::Cache.eq(cache.id))
                .one(&state.web_db)
                .await?
                .or_not_found("Team grant")?,
        ),
    };

    let allowed = has_cache_permission(
        &state,
        user.id,
        cache.id,
        CachePermission::ManageCacheMembers,
        api_key.as_ref(),
    )
    .await?
        || removes_as_team_admin(&state, &user, &api_key, team.id).await?;
    if !allowed {
        return Err(WebError::not_found("Team grant"));
    }
    if cache.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.",
        ));
    }

    let Some(grant) = grant else {
        if let Some(request) = request {
            request.into_active_model().delete(&state.web_db).await?;
        }
        return Ok(ok_json("Request withdrawn".to_string()));
    };
    grant.into_active_model().delete(&state.web_db).await?;

    audit_record(
        &state,
        Some(user.id),
        Action::TeamGrantRemove,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(
            serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() }),
        ),
    )
    .await;
    Ok(ok_json("Team grant removed".to_string()))
}
