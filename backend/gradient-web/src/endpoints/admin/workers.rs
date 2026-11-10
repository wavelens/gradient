/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::endpoints::board_subjects::{BoardProject, board_projects};
use crate::endpoints::projects::workers::{WorkerConnection, worker_connection};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;

#[derive(Serialize, Clone, utoipa::ToSchema)]
pub struct WorkerOwningTeam {
    pub id: TeamId,
    pub name: String,
    pub display_name: String,
    pub active: bool,
    pub managed: bool,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
pub struct WorkerProjectRegistration {
    pub project: BoardProject,
    pub display_name: String,
    pub active: bool,
    pub managed: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AdminWorkerListEntry {
    pub worker_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<WorkerOwningTeam>,
    pub registrations: Vec<WorkerProjectRegistration>,
    pub projects: Vec<BoardProject>,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerInfo>,
    pub requested_by_admin: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateWorkerRequest {
    pub draining: Option<bool>,
}

struct WorkerRows {
    team_workers: Vec<(MTeamWorker, MTeam)>,
    registrations: Vec<MWorkerRegistration>,
    team_projects: HashMap<TeamId, Vec<ProjectId>>,
    projects: HashMap<Uuid, BoardProject>,
}

fn build_admin_worker_list_entries_by_worker_id(
    rows: WorkerRows,
    live: HashMap<String, WorkerInfo>,
    scheduler: &Scheduler,
) -> Vec<AdminWorkerListEntry> {
    let project = |id: ProjectId| rows.projects.get(&id.into()).cloned();
    let worker_ids: BTreeSet<&str> = rows
        .team_workers
        .iter()
        .map(|(worker, _)| worker.worker_id.as_str())
        .chain(rows.registrations.iter().map(|row| row.worker_id.as_str()))
        .chain(live.keys().map(String::as_str))
        .collect();
    let mut entries: BTreeMap<String, AdminWorkerListEntry> = worker_ids
        .into_iter()
        .map(|worker_id| {
            let entry = AdminWorkerListEntry {
                worker_id: worker_id.to_owned(),
                display_name: None,
                team: None,
                registrations: Vec::new(),
                projects: Vec::new(),
                connection: worker_connection(
                    &live,
                    Some(&scheduler.connection_failures),
                    worker_id,
                ),
                live: live.get(worker_id).cloned(),
                requested_by_admin: scheduler.drained_workers.contains(worker_id),
            };
            (worker_id.to_owned(), entry)
        })
        .collect();

    for (worker, team) in &rows.team_workers {
        let served = rows.team_projects.get(&team.id).into_iter().flatten();
        let projects = served.copied().filter_map(project).collect();
        let Some(entry) = entries.get_mut(&worker.worker_id) else {
            continue;
        };
        entry.display_name = Some(worker.display_name.clone());
        entry.team = Some(WorkerOwningTeam {
            id: team.id,
            name: team.name.clone(),
            display_name: team.display_name.clone(),
            active: worker.active,
            managed: worker.managed,
        });
        entry.projects = projects;
    }
    for registration in &rows.registrations {
        let Some(project) = project(registration.peer_id) else {
            continue;
        };
        let Some(entry) = entries.get_mut(&registration.worker_id) else {
            continue;
        };
        entry
            .display_name
            .get_or_insert_with(|| registration.display_name.clone());
        if !entry.projects.iter().any(|served| served.id == project.id) {
            entry.projects.push(project.clone());
        }
        entry.registrations.push(WorkerProjectRegistration {
            project,
            display_name: registration.display_name.clone(),
            active: registration.active,
            managed: registration.managed,
        });
    }
    entries.into_values().collect()
}

async fn load_full_admin_worker_list(
    db: &WebDb,
    scheduler: &Scheduler,
) -> WebResult<Vec<AdminWorkerListEntry>> {
    let team_workers = gradient_db::teams::workers::load_all_workers_with_team(db).await?;
    let registrations = gradient_db::worker_registrations::all_worker_registrations(db).await?;
    let team_projects =
        gradient_db::teams::workers::team_id_to_projects_granted_with_workers(db).await?;
    let project_ids: Vec<Uuid> = team_projects
        .values()
        .flatten()
        .copied()
        .chain(
            registrations
                .iter()
                .map(|registration| registration.peer_id),
        )
        .map(Into::into)
        .collect();
    let projects = board_projects(db, &project_ids).await?;
    let live = scheduler
        .workers_info()
        .await
        .into_iter()
        .map(|worker| (worker.id.clone(), worker))
        .collect();

    Ok(build_admin_worker_list_entries_by_worker_id(
        WorkerRows {
            team_workers,
            registrations,
            team_projects,
            projects,
        },
        live,
        scheduler,
    ))
}

pub(crate) async fn start_or_cancel_worker_admin_drain(
    scheduler: &Scheduler,
    worker_id: &str,
    draining: bool,
) {
    if draining {
        scheduler.mark_worker_draining_by_admin(worker_id).await;
    } else {
        scheduler.cancel_worker_admin_drain(worker_id).await;
    }
}

async fn find_admin_worker(
    db: &WebDb,
    scheduler: &Scheduler,
    worker_id: &str,
) -> WebResult<AdminWorkerListEntry> {
    load_full_admin_worker_list(db, scheduler)
        .await?
        .into_iter()
        .find(|entry| entry.worker_id == worker_id)
        .ok_or_else(|| WebError::not_found(Resource::Worker))
}

#[utoipa::path(
    get,
    path = "/workers",
    operation_id = "listWorkers",
    tag = "admin",
    summary = "List workers",
    description = "List every worker in the instance. Team workers, project registrations and connected workers without registration all appear here. Entries list the owning team, project registration, the projects that belong to the worker, connection status and live info for connected workers. An admin drain can keep `requested_by_admin` true while active. Reconnects do not clear the drain. An admin cancel or a server restart can clear the drain. Requests need `superuser`.",
    responses((status = OK, description = "Worker List", body = Vec<AdminWorkerListEntry>), WebError)
)]
pub async fn get_workers(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<AdminWorkerListEntry>>>> {
    Ok(ok_json(
        load_full_admin_worker_list(&state.web_db, &scheduler).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/workers/{worker}",
    operation_id = "getWorker",
    tag = "admin",
    summary = "Get worker",
    description = "Return one worker in the same format as `listWorkers`. Return 404 if the worker is neither registered nor connected. Require `superuser`.",
    params(("worker" = String, Path)),
    responses((status = OK, description = "Worker", body = AdminWorkerListEntry), WebError)
)]
pub async fn get_worker(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(worker): Path<String>,
) -> WebResult<Json<BaseResponse<AdminWorkerListEntry>>> {
    Ok(ok_json(
        find_admin_worker(&state.web_db, &scheduler, &worker).await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/workers/{worker}",
    operation_id = "updateWorker",
    tag = "admin",
    summary = "Drain or undrain worker",
    description = "Start an admin drain (`draining: true`) or cancel the drain (`draining: false`). Drained workers receive no new jobs. They finish running jobs and keep the drain state on reconnect until an admin can cancel the drain or the server can restart. Return the worker as `listWorkers`. Add `superuser` to the request.",
    params(("worker" = String, Path)),
    request_body = UpdateWorkerRequest,
    responses((status = OK, description = "Worker", body = AdminWorkerListEntry), WebError)
)]
pub async fn update_worker(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(worker): Path<String>,
    Json(body): Json<UpdateWorkerRequest>,
) -> WebResult<Json<BaseResponse<AdminWorkerListEntry>>> {
    find_admin_worker(&state.web_db, &scheduler, &worker).await?;
    if let Some(draining) = body.draining {
        start_or_cancel_worker_admin_drain(&scheduler, &worker, draining).await;
    }
    Ok(ok_json(
        find_admin_worker(&state.web_db, &scheduler, &worker).await?,
    ))
}
