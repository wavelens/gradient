/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::settings::{CacheSubscriptionItem, cache_subscription_items};
use crate::access::has_permission;
use crate::api_spec::ProjectPath;
use crate::endpoints::tasks::error_message_if_caller_can_edit_task;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{Member, ProjectScope};
use axum::Json;
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
use gradient_types::permissions::Permission;
use gradient_types::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectReadinessResponse {
    pub ready: bool,
    pub workers: ProjectReadinessWorkers,
    pub cache: ProjectSubscriptionReadinessCache,
    pub repository: GitAccessReadiness,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectReadinessWorkers {
    pub ready: bool,
    pub registered: usize,
    pub teams: Vec<TeamReadinessEntry>,
    pub teams_granted_without_workers: Vec<TeamReadinessEntry>,
    pub teams_you_can_grant: Vec<TeamReadinessEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamReadinessEntry {
    pub name: String,
    pub display_name: String,
    pub workers: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectSubscriptionReadinessCache {
    pub ready: bool,
    pub subscriptions: Vec<CacheSubscriptionItem>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct GitAccessReadiness {
    pub status: TaskRepositoryFetchStatus,
    pub tasks: Vec<ProjectTaskReadinessEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectTaskReadinessEntry {
    pub name: String,
    pub display_name: String,
    pub status: TaskRepositoryFetchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskRepositoryFetchStatus {
    Verified,
    Unchecked,
    Failing,
}

impl TaskRepositoryFetchStatus {
    fn of(task: &MTask) -> Self {
        match (task.last_fetch_at, &task.last_fetch_error) {
            (None, _) => Self::Unchecked,
            (Some(_), Some(_)) => Self::Failing,
            (Some(_), None) => Self::Verified,
        }
    }
}

async fn active_worker_counts(db: &WebDb, teams: &[TeamId]) -> WebResult<HashMap<TeamId, usize>> {
    let mut counts = HashMap::new();
    for team in teams {
        let workers = gradient_db::teams::workers::workers_of(db, *team).await?;
        counts.insert(*team, workers.iter().filter(|w| w.active).count());
    }
    Ok(counts)
}

async fn worker_readiness(
    db: &WebDb,
    project: ProjectId,
    caller: &MUser,
    manages_workers: bool,
) -> WebResult<ProjectReadinessWorkers> {
    let grants = gradient_db::teams::grants::grants_of_project(db, project).await?;
    let caller_teams: Vec<MTeam> = if manages_workers {
        gradient_db::teams::members::teams_of_user(db, caller.id)
            .await?
            .into_iter()
            .filter_map(|(_, team)| team)
            .collect()
    } else {
        Vec::new()
    };
    let granted_with_workers: HashSet<TeamId> = grants
        .iter()
        .filter(|g| g.includes_workers)
        .map(|g| g.team)
        .collect();
    let team_ids: Vec<TeamId> = grants
        .iter()
        .map(|g| g.team)
        .chain(caller_teams.iter().map(|t| t.id))
        .collect();
    let mut teams: HashMap<TeamId, MTeam> = by_ids::<ETeam, _>(db, &team_ids).await?;
    teams.extend(caller_teams.iter().map(|t| (t.id, t.clone())));
    let counts = active_worker_counts(db, &team_ids).await?;
    let entry = |team: TeamId| {
        teams.get(&team).map(|t| TeamReadinessEntry {
            name: t.name.clone(),
            display_name: t.display_name.clone(),
            workers: counts.get(&team).copied().unwrap_or_default(),
        })
    };

    let registered = gradient_db::worker_registrations::of_project(db, project)
        .await?
        .iter()
        .filter(|r| r.active)
        .count();
    let granted = |with_workers: bool| -> Vec<TeamReadinessEntry> {
        grants
            .iter()
            .filter(|g| g.includes_workers == with_workers)
            .filter_map(|g| entry(g.team))
            .collect()
    };
    let teams_you_can_grant = caller_teams
        .iter()
        .filter(|t| !granted_with_workers.contains(&t.id))
        .filter_map(|t| entry(t.id))
        .filter(|t| t.workers > 0)
        .collect();

    Ok(ProjectReadinessWorkers {
        ready: gradient_db::projects::workers::project_has_eval_capable_worker_registration(
            db, project,
        )
        .await?,
        registered,
        teams: granted(true),
        teams_granted_without_workers: granted(false),
        teams_you_can_grant,
    })
}

async fn repository_readiness(
    db: &WebDb,
    project: ProjectId,
    can_edit: bool,
) -> WebResult<GitAccessReadiness> {
    let tasks: Vec<ProjectTaskReadinessEntry> = gradient_db::tasks::of_projects(db, vec![project])
        .await?
        .into_iter()
        .map(|task| ProjectTaskReadinessEntry {
            status: TaskRepositoryFetchStatus::of(&task),
            error: error_message_if_caller_can_edit_task(task.last_fetch_error, can_edit),
            name: task.name,
            display_name: task.display_name,
        })
        .collect();
    let status = tasks
        .iter()
        .map(|t| t.status)
        .max()
        .unwrap_or(TaskRepositoryFetchStatus::Unchecked);
    Ok(GitAccessReadiness { status, tasks })
}

#[utoipa::path(
    get,
    path = "/projects/{project}/readiness",
    operation_id = "getProjectReadiness",
    tag = "projects",
    summary = "Project Readiness",
    description = "Show what this project needs before an evaluation run. These items include workers that can run evaluations, a cache subscription with write access, and repository access for each task. Only callers with `manageWorkers` can view the list of teams that the caller can grant with workers. Only callers with `editTask` can view repository errors.",
    params(ProjectPath),
    responses((status = OK, description = "OK", body = ProjectReadinessResponse), WebError)
)]
pub async fn get_project_readiness(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        ..
    }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<ProjectReadinessResponse>>> {
    let allowed = |permission| {
        let api_key = api_key.as_ref();
        let db = &db.web_db;
        let user = &user;
        async move {
            Ok::<_, WebError>(
                user.superuser
                    || has_permission(db, user.id, project.id, permission, api_key).await?,
            )
        }
    };
    let manages_workers = allowed(Permission::ManageWorkers).await?;
    let edits_tasks = allowed(Permission::EditTask).await?;

    let workers = worker_readiness(&db.web_db, project.id, &user, manages_workers).await?;
    let cache = ProjectSubscriptionReadinessCache {
        ready: gradient_db::projects::caches::project_has_writable_cache(&db.web_db, project.id)
            .await?,
        subscriptions: cache_subscription_items(&db.web_db, project.id).await?,
    };
    let repository = repository_readiness(&db.web_db, project.id, edits_tasks).await?;

    Ok(ok_json(ProjectReadinessResponse {
        ready: workers.ready
            && cache.ready
            && repository.status != TaskRepositoryFetchStatus::Failing,
        workers,
        cache,
        repository,
    }))
}
