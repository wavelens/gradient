/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use crate::access::{Caller, ProjectAccess, has_cache_permission, has_permission, load_project};
use crate::audit::Audit;
use crate::authorization::MaybeApiKey;
use crate::endpoints::teams::workers::{reauth_team_workers, withdraw_team_workers};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::{CachePermission, Permission};
use crate::refusal::{Change, SharedRefusal};
use crate::resource::Resource;
use crate::scope::{CacheScope, ProjectScope, Readable, Requires, RequiresUnmanaged, can};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::lookup::{by_ids, get_any_cache_by_name, get_any_project_by_name};
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
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

impl TeamGrantEntry {
    fn new(team: &MTeam, role: Option<String>, users: bool, workers: bool, pending: bool) -> Self {
        Self {
            team: team.name.clone(),
            display_name: team.display_name.clone(),
            role,
            users,
            workers,
            pending,
        }
    }
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

async fn find_team(db: &WebDb, name: &str) -> WebResult<MTeam> {
    gradient_db::teams::by_name(db, name)
        .await?
        .or_not_found(Resource::Team)
}

fn grant_permission(includes_users: bool) -> Permission {
    if includes_users {
        Permission::ManageMembers
    } else {
        Permission::ManageWorkers
    }
}

async fn removes_as_team_admin(
    db: &WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    team: TeamId,
) -> WebResult<bool> {
    let pinned = api_key
        .as_ref()
        .is_some_and(|k| k.project.is_some() || k.cache_pin.is_some());
    Ok(!pinned && is_team_admin(db, user, team).await?)
}

async fn is_team_admin(db: &WebDb, user: &MUser, team: TeamId) -> WebResult<bool> {
    Ok(user.superuser
        || gradient_db::teams::team_role_of(db, team, user.id).await? == Some(TeamRole::Admin))
}

async fn project_role(db: &WebDb, project: ProjectId, name: &str) -> WebResult<RoleId> {
    Ok(
        gradient_db::roles::by_name::<ERole, _>(db, CRole::Name, name, CRole::Project, project)
            .await?
            .or_not_found(Resource::Role)?
            .id,
    )
}

async fn cache_role(db: &WebDb, cache: CacheId, name: &str) -> WebResult<RoleId> {
    Ok(gradient_db::roles::by_name::<ECacheRole, _>(
        db,
        CCacheRole::Name,
        name,
        CCacheRole::Cache,
        cache,
    )
    .await?
    .or_not_found(Resource::Role)?
    .id)
}

pub(crate) async fn grant_workers(
    db: &WebDb,
    scheduler: &Scheduler,
    team: TeamId,
    project: ProjectId,
) -> WebResult<()> {
    let workers = gradient_db::teams::team_worker_ids(db, team).await?;
    reauth_team_workers(scheduler, &workers).await;
    if let Err(e) = gradient_ci::unpark_no_workers_for_project(db, project).await {
        tracing::warn!(error = %e, project_id = %project, "failed to unpark evaluations after a team grant");
    }
    Ok(())
}

async fn take_workers(
    db: &WebDb,
    scheduler: &Scheduler,
    team: TeamId,
    project: ProjectId,
) -> WebResult<()> {
    let workers = gradient_db::teams::team_worker_ids(db, team).await?;
    withdraw_team_workers(scheduler, &workers, &HashSet::from([project])).await;
    Ok(())
}

fn project_role_name(roles: &HashMap<RoleId, String>, role: Option<RoleId>) -> Option<String> {
    role.and_then(|id| roles.get(&id).cloned())
}

pub async fn get_project_teams(
    ProjectScope { project, db, .. }: ProjectScope<Readable>,
) -> WebResult<Json<BaseResponse<Vec<TeamGrantEntry>>>> {
    let grants = gradient_db::teams::grants::grants_of_project(&db.web_db, project.id).await?;
    let requests =
        gradient_db::teams::requests::requests_of_project(&db.web_db, project.id).await?;

    let team_ids: Vec<TeamId> = grants
        .iter()
        .map(|g| g.team)
        .chain(requests.iter().map(|r| r.team))
        .collect();
    let teams: HashMap<TeamId, MTeam> = by_ids::<ETeam, _>(&db.web_db, &team_ids).await?;
    let role_ids: Vec<RoleId> = grants
        .iter()
        .filter_map(|g| g.role)
        .chain(requests.iter().filter_map(|r| r.role))
        .collect();
    let roles = crate::helpers::role_names(&db.web_db, role_ids).await?;

    let entry = |team: TeamId, role, users, workers, pending| {
        teams.get(&team).map(|team| {
            TeamGrantEntry::new(
                team,
                project_role_name(&roles, role),
                users,
                workers,
                pending,
            )
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
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(project): Path<String>,
    Json(body): Json<GrantProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let db = state.db();
    if !body.users && !body.workers {
        return Err(TeamRefusal::EmptyGrant.into());
    }
    let permission = grant_permission(body.users);
    let project = load_project(
        &db,
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
            &state.web_db,
            user.id,
            project.id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?
    {
        return Err(SharedRefusal::MissingPermission.into());
    }

    let team = find_team(&state.web_db, &body.team).await?;
    let role = match (body.users, body.role.as_deref()) {
        (true, Some(name)) => Some(project_role(&state.web_db, project.id, name).await?),
        (true, None) => {
            return Err(TeamRefusal::RoleRequiredForUsers.into());
        }
        (false, _) => None,
    };
    if gradient_db::teams::grants::project_grant(&state.web_db, team.id, project.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamGrant));
    }

    let payload =
        serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() });
    let owner = EventOwner::project(project.id);

    if !is_team_admin(&state.web_db, &user, team.id).await? {
        gradient_db::teams::requests::add_project_request(
            &state.web_db,
            MTeamProjectRequest {
                id: TeamProjectRequestId::now_v7(),
                team: team.id,
                project: project.id,
                role,
                includes_users: body.users,
                includes_workers: body.workers,
                requested_by: Some(user.id),
                created_at: gradient_types::now(),
            },
        )
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TeamRequest))?;
        audit.write(Action::TeamRequestCreate, owner, payload).await;
        return Ok(ok_json("Request sent".to_string()));
    }

    gradient_db::teams::grants::grant_project(
        &state.web_db,
        MTeamProject {
            id: TeamProjectId::now_v7(),
            team: team.id,
            project: project.id,
            role,
            includes_users: body.users,
            includes_workers: body.workers,
            managed: false,
            created_at: gradient_types::now(),
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;
    if body.workers {
        grant_workers(&state.web_db, &scheduler, team.id, project.id).await?;
    }

    audit.write(Action::TeamGrantCreate, owner, payload).await;
    Ok(ok_json("Team granted".to_string()))
}

pub async fn patch_project_team(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((project, team)): Path<(String, String)>,
    Json(body): Json<PatchProjectGrantRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let db = state.db();
    let permission = if body.workers.is_some() && body.users.is_none() && body.role.is_none() {
        Permission::ManageWorkers
    } else {
        Permission::ManageMembers
    };
    let project = load_project(
        &db,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Require {
            permission,
            reject_managed: true,
        },
    )
    .await?;
    let team = find_team(&state.web_db, &team).await?;
    let grant = gradient_db::teams::grants::project_grant(&state.web_db, team.id, project.id)
        .await?
        .or_not_found(Resource::TeamGrant)?;

    let users = body.users.unwrap_or(grant.includes_users);
    let workers = body.workers.unwrap_or(grant.includes_workers);
    if !users && !workers {
        return Err(TeamRefusal::EmptyGrantAfterUpdate.into());
    }
    if workers && !grant.includes_workers && !is_team_admin(&state.web_db, &user, team.id).await? {
        return Err(TeamRefusal::WorkersNeedTeamAdmin.into());
    }
    let role = match (users, body.role.as_deref()) {
        (true, Some(name)) => Some(project_role(&state.web_db, project.id, name).await?),
        (true, None) => grant
            .role
            .ok_or(TeamRefusal::RoleRequiredForUsers)
            .map(Some)?,
        (false, _) => None,
    };

    let had_workers = grant.includes_workers;
    let read = grant.clone();
    let mut active: ATeamProject = grant.into();
    active.includes_users = Set(users);
    active.includes_workers = Set(workers);
    active.role = Set(role);
    gradient_db::teams::grants::update_project_grant(&state.web_db, active, &read)
        .await?
        .or_not_found(Resource::TeamGrant)?;

    match (had_workers, workers) {
        (false, true) => grant_workers(&state.web_db, &scheduler, team.id, project.id).await?,
        (true, false) => take_workers(&state.web_db, &scheduler, team.id, project.id).await?,
        _ => {}
    }

    audit.write(Action::TeamGrantUpdate, EventOwner::project(project.id), serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() }))
    .await;
    Ok(ok_json("Team grant updated".to_string()))
}

pub async fn delete_project_team(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((project, team)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let db = state.db();
    let project = get_any_project_by_name(&db, project)
        .await?
        .or_not_found(Resource::Project)?;
    let team = find_team(&state.web_db, &team).await?;
    let grant =
        gradient_db::teams::grants::project_grant(&state.web_db, team.id, project.id).await?;
    let (request, includes_users) = match &grant {
        Some(grant) => (None, grant.includes_users),
        None => {
            let request =
                gradient_db::teams::requests::project_request(&state.web_db, team.id, project.id)
                    .await?
                    .or_not_found(Resource::TeamGrant)?;
            let includes_users = request.includes_users;
            (Some(request), includes_users)
        }
    };

    let allowed = has_permission(
        &state.web_db,
        user.id,
        project.id,
        grant_permission(includes_users),
        api_key.as_ref(),
    )
    .await?
        || removes_as_team_admin(&state.web_db, &user, &api_key, team.id).await?;
    if !allowed {
        return Err(WebError::not_found(Resource::TeamGrant));
    }
    if project.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Project).into());
    }

    let Some(grant) = grant else {
        if let Some(request) = request {
            gradient_db::teams::requests::remove_project_request(&state.web_db, request).await?;
        }
        return Ok(ok_json("Request withdrawn".to_string()));
    };
    let had_workers = grant.includes_workers;
    gradient_db::teams::grants::revoke_project_grant(&state.web_db, grant).await?;
    if had_workers {
        take_workers(&state.web_db, &scheduler, team.id, project.id).await?;
    }

    audit.write(Action::TeamGrantRemove, EventOwner::project(project.id), serde_json::json!({ "team_id": team.id.to_string(), "project_id": project.id.to_string() }))
    .await;
    Ok(ok_json("Team grant removed".to_string()))
}

fn cache_role_name(roles: &HashMap<RoleId, String>, role: RoleId) -> Option<String> {
    roles.get(&role).cloned()
}

pub async fn get_cache_teams(
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ViewCache>>,
) -> WebResult<Json<BaseResponse<Vec<TeamGrantEntry>>>> {
    let grants = gradient_db::teams::grants::grants_of_cache(&db.web_db, cache.id).await?;
    let requests = gradient_db::teams::requests::requests_of_cache(&db.web_db, cache.id).await?;

    let team_ids: Vec<TeamId> = grants
        .iter()
        .map(|g| g.team)
        .chain(requests.iter().map(|r| r.team))
        .collect();
    let teams: HashMap<TeamId, MTeam> = by_ids::<ETeam, _>(&db.web_db, &team_ids).await?;
    let role_ids: Vec<RoleId> = grants
        .iter()
        .map(|g| g.role)
        .chain(requests.iter().map(|r| r.role))
        .collect();
    let roles: HashMap<RoleId, String> = by_ids::<ECacheRole, _>(&db.web_db, &role_ids)
        .await?
        .into_iter()
        .map(|(id, role)| (id, role.name))
        .collect();

    let entry = |team: TeamId, role, pending| {
        teams.get(&team).map(|team| {
            TeamGrantEntry::new(team, cache_role_name(&roles, role), true, false, pending)
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
    CacheScope {
        cache, user, db, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    audit: Audit,
    Json(body): Json<GrantCacheRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let team = find_team(&db.web_db, &body.team).await?;
    let role = cache_role(&db.web_db, cache.id, &body.role).await?;
    if gradient_db::teams::grants::cache_grant(&db.web_db, team.id, cache.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::TeamGrant));
    }

    let payload =
        serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() });
    let owner = EventOwner::cache(cache.id);

    if !is_team_admin(&db.web_db, &user, team.id).await? {
        gradient_db::teams::requests::add_cache_request(
            &db.web_db,
            MTeamCacheRequest {
                id: TeamCacheRequestId::now_v7(),
                team: team.id,
                cache: cache.id,
                role,
                requested_by: Some(user.id),
                created_at: gradient_types::now(),
            },
        )
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::TeamRequest))?;
        audit.write(Action::TeamRequestCreate, owner, payload).await;
        return Ok(ok_json("Request sent".to_string()));
    }

    gradient_db::teams::grants::grant_cache(
        &db.web_db,
        MTeamCache {
            id: TeamCacheId::now_v7(),
            team: team.id,
            cache: cache.id,
            role,
            managed: false,
            created_at: gradient_types::now(),
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::TeamGrant))?;

    audit.write(Action::TeamGrantCreate, owner, payload).await;
    Ok(ok_json("Team granted".to_string()))
}

pub async fn patch_cache_team(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    audit: Audit,
    Json(body): Json<PatchCacheGrantRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let team: String = params.param("team")?;

    let team = find_team(&db.web_db, &team).await?;
    let grant = gradient_db::teams::grants::cache_grant(&db.web_db, team.id, cache.id)
        .await?
        .or_not_found(Resource::TeamGrant)?;
    let role = cache_role(&db.web_db, cache.id, &body.role).await?;

    let mut active: ATeamCache = grant.into();
    active.role = Set(role);
    gradient_db::teams::grants::update_cache_grant(&db.web_db, active).await?;

    audit
        .write(
            Action::TeamGrantUpdate,
            EventOwner::cache(cache.id),
            serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() }),
        )
        .await;
    Ok(ok_json("Team grant updated".to_string()))
}

pub async fn delete_cache_team(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache, team)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let db = state.db();
    let cache = get_any_cache_by_name(&db, cache)
        .await?
        .or_not_found(Resource::Cache)?;
    let team = find_team(&state.web_db, &team).await?;
    let grant = gradient_db::teams::grants::cache_grant(&state.web_db, team.id, cache.id).await?;
    let request = match grant {
        Some(_) => None,
        None => Some(
            gradient_db::teams::requests::cache_request(&state.web_db, team.id, cache.id)
                .await?
                .or_not_found(Resource::TeamGrant)?,
        ),
    };

    let allowed = has_cache_permission(
        &state.web_db,
        user.id,
        cache.id,
        CachePermission::ManageCacheMembers,
        api_key.as_ref(),
    )
    .await?
        || removes_as_team_admin(&state.web_db, &user, &api_key, team.id).await?;
    if !allowed {
        return Err(WebError::not_found(Resource::TeamGrant));
    }
    if cache.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Cache).into());
    }

    let Some(grant) = grant else {
        if let Some(request) = request {
            gradient_db::teams::requests::remove_cache_request(&state.web_db, request).await?;
        }
        return Ok(ok_json("Request withdrawn".to_string()));
    };
    gradient_db::teams::grants::revoke_cache_grant(&state.web_db, grant).await?;

    audit
        .write(
            Action::TeamGrantRemove,
            EventOwner::cache(cache.id),
            serde_json::json!({ "team_id": team.id.to_string(), "cache_id": cache.id.to_string() }),
        )
        .await;
    Ok(ok_json("Team grant removed".to_string()))
}
