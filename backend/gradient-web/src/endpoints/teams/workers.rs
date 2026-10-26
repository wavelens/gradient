/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TeamRefusal;
use crate::api_spec::TeamPath;
use crate::audit::Audit;
use crate::endpoints::projects::workers::{
    PatchWorkerRequest, RegisterWorkerRequest, WorkerConnection, WorkerLiveInfo,
    encrypt_for_dialing, live_workers, patch_edits_managed_fields, worker_connection,
};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Admin, Member, TeamScope};
use axum::extract::State;
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, utoipa::ToSchema)]
pub struct TeamWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub gradient_ci: bool,
    pub enable_network: bool,
    pub enable_secrets: bool,
    pub enable_exec: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
}

impl TeamWorkerEntry {
    fn new(
        worker: MTeamWorker,
        connection: WorkerConnection,
        live: Option<WorkerLiveInfo>,
    ) -> Self {
        Self {
            worker_id: worker.worker_id,
            display_name: worker.display_name,
            registered_at: worker.created_at,
            active: worker.active,
            managed: worker.managed,
            url: worker.url,
            gradient_ci: worker.gradient_ci,
            enable_network: worker.enable_network,
            enable_secrets: worker.enable_secrets,
            enable_exec: worker.enable_exec,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
            connection,
            live,
        }
    }
}

fn team_worker_live(live: &HashMap<String, WorkerInfo>, worker_id: &str) -> Option<WorkerLiveInfo> {
    live.get(worker_id).map(WorkerLiveInfo::from)
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegisteredTeamWorker {
    pub team: TeamId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

pub async fn reauth_team_workers(scheduler: &Scheduler, workers: &[String]) {
    for worker_id in workers {
        scheduler.request_reauth(worker_id).await;
    }
}

pub async fn withdraw_team_workers(
    scheduler: &Scheduler,
    workers: &[String],
    projects: &HashSet<ProjectId>,
) {
    for worker_id in workers {
        scheduler
            .abort_project_jobs_on_worker(worker_id, projects)
            .await;
        scheduler.request_reauth(worker_id).await;
    }
}

async fn granted_projects(db: &WebDb, team: TeamId) -> WebResult<HashSet<ProjectId>> {
    Ok(
        gradient_db::teams::workers::projects_granted_with_workers(db, team)
            .await?
            .into_iter()
            .collect(),
    )
}

async fn find_team_worker(db: &WebDb, team: TeamId, worker_id: &str) -> WebResult<MTeamWorker> {
    gradient_db::teams::workers::worker_of_team(db, team, worker_id)
        .await?
        .or_not_found(Resource::TeamWorker)
}

#[utoipa::path(
    get,
    path = "/teams/{team}/workers",
    operation_id = "listTeamWorkers",
    tag = "teams",
    summary = "List team workers",
    description = "A team worker serves every project that grants the team its workers. `last_error` is shown to team Admins.",
    params(TeamPath),
    responses((status = OK, description = "OK", body = Vec<TeamWorkerEntry>), WebError)
)]
pub async fn get_team_workers(
    TeamScope {
        team,
        role,
        user,
        db,
        ..
    }: TeamScope<Member>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<TeamWorkerEntry>>>> {
    let failures = (user.superuser || role == Some(TeamRole::Admin))
        .then_some(&*scheduler.connection_failures);
    let live_workers = live_workers(scheduler.workers_info().await);
    let rows = gradient_db::teams::workers::workers_of(&db.web_db, team.id).await?;

    let mut entries = Vec::with_capacity(rows.len());
    for worker in rows {
        let connection = worker_connection(&live_workers, failures, &worker.worker_id);
        let live = team_worker_live(&live_workers, &worker.worker_id);
        entries.push(TeamWorkerEntry::new(worker, connection, live));
    }

    Ok(ok_json(entries))
}

#[utoipa::path(
    post,
    path = "/teams/{team}/workers",
    operation_id = "registerTeamWorker",
    tag = "teams",
    summary = "Register team worker",
    description = "Needs Admin in the team. The worker answers the server challenge with `<team id>:<token>` or `*:<token>` in its peers file. `token` is returned only when the server generated it.",
    params(TeamPath),
    request_body = RegisterWorkerRequest,
    responses((status = OK, description = "OK", body = RegisteredTeamWorker), WebError)
)]
pub async fn post_team_worker(
    TeamScope { team, user, .. }: TeamScope<Admin>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisteredTeamWorker>>> {
    let worker_id = Uuid::parse_str(&body.worker_id)
        .map_err(|_| SharedRefusal::InvalidWorkerId)?
        .to_string();
    let registered = gradient_db::worker_registrations::by_worker_id(&state.web_db, &worker_id)
        .await?
        .is_some();
    if registered || gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id).await? {
        return Err(TeamRefusal::WorkerIdRegistered.into());
    }

    let (token, return_token) = crate::endpoints::worker_tokens::issue(body.token)?;
    let token_encrypted = encrypt_for_dialing(
        &state.config.secrets.crypt_file,
        body.url.as_deref(),
        &token,
    )?;

    gradient_db::teams::workers::register(
        &state.web_db,
        MTeamWorker {
            id: TeamWorkerId::now_v7(),
            team: team.id,
            worker_id: worker_id.clone(),
            token_hash: password_auth::generate_hash(&token),
            token_encrypted,
            url: body.url,
            display_name: body.display_name.trim().to_string(),
            gradient_ci: false,
            enable_network: body.enable_network,
            enable_secrets: body.enable_secrets,
            enable_exec: body.enable_exec,
            enable_eval: body.enable_eval,
            enable_build: body.enable_build,
            active: true,
            managed: false,
            created_by: Some(user.id),
            created_at: gradient_types::now(),
        },
    )
    .await
    .map_err(|e| WebError::from_unique_violation(e, TeamRefusal::WorkerIdRegistered))?;

    scheduler.request_reauth(&worker_id).await;
    for project in granted_projects(&state.web_db, team.id).await? {
        if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project).await {
            tracing::warn!(error = %e, project_id = %project, "failed to unpark evaluations after a team worker registration");
        }
    }

    audit
        .write(
            Action::TeamWorkerCreate,
            EventOwner::team(team.id),
            serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id }),
        )
        .await;

    Ok(ok_json(RegisteredTeamWorker {
        team: team.id,
        token: return_token.then_some(token),
    }))
}

#[utoipa::path(
    patch,
    path = "/teams/{team}/workers/{worker_id}",
    operation_id = "patchTeamWorker",
    tag = "teams",
    summary = "Change team worker",
    description = "Needs Admin in the team. Deactivating aborts the worker's running jobs of the granted projects.",
    params(TeamPath, ("worker_id" = String, Path)),
    request_body = PatchWorkerRequest,
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_team_worker(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    let worker = find_team_worker(&db.web_db, team.id, &worker_id).await?;
    if worker.managed && patch_edits_managed_fields(&body) {
        return Err(SharedRefusal::WorkerManagedByState.into());
    }

    let mut active: ATeamWorker = worker.into();
    if let Some(value) = body.active {
        active.active = Set(value);
    }
    if let Some(name) = body.display_name {
        active.display_name = Set(name.trim().to_string());
    }
    if let Some(value) = body.enable_network {
        active.enable_network = Set(value);
    }
    if let Some(value) = body.enable_secrets {
        active.enable_secrets = Set(value);
    }
    if let Some(value) = body.enable_exec {
        active.enable_exec = Set(value);
    }
    if let Some(value) = body.enable_eval {
        active.enable_eval = Set(value);
    }
    if let Some(value) = body.enable_build {
        active.enable_build = Set(value);
    }
    gradient_db::teams::workers::update(&db.web_db, active).await?;

    let workers = [worker_id.clone()];
    if body.active == Some(false) {
        withdraw_team_workers(
            &scheduler,
            &workers,
            &granted_projects(&db.web_db, team.id).await?,
        )
        .await;
    } else {
        reauth_team_workers(&scheduler, &workers).await;
    }

    audit
        .write(
            Action::TeamWorkerUpdate,
            EventOwner::team(team.id),
            serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id }),
        )
        .await;

    Ok(ok_json("ok".to_string()))
}

#[utoipa::path(
    delete,
    path = "/teams/{team}/workers/{worker_id}",
    operation_id = "deleteTeamWorker",
    tag = "teams",
    summary = "Delete team worker",
    description = "Needs Admin in the team.",
    params(TeamPath, ("worker_id" = String, Path)),
    responses((status = OK, description = "OK", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_team_worker(
    TeamScope {
        team, db, params, ..
    }: TeamScope<Admin>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    let worker = find_team_worker(&db.web_db, team.id, &worker_id).await?;
    if worker.managed {
        return Err(SharedRefusal::WorkerManagedByState.into());
    }

    let projects = granted_projects(&db.web_db, team.id).await?;
    gradient_db::teams::workers::unregister(&db.web_db, worker).await?;
    withdraw_team_workers(&scheduler, std::slice::from_ref(&worker_id), &projects).await;
    scheduler.connection_failures.clear(&worker_id);

    audit
        .write(
            Action::TeamWorkerDelete,
            EventOwner::team(team.id),
            serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id }),
        )
        .await;

    Ok(ok_json(format!("worker '{}' unregistered", worker_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(id: &str) -> WorkerInfo {
        WorkerInfo {
            id: id.into(),
            capabilities: Default::default(),
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
            max_concurrent_builds: 4,
            assigned_job_count: 0,
            assigned_build_count: 0,
            draining: false,
            authorized_peers: None,
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 16384,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
            runner: None,
        }
    }

    #[test]
    fn connected_team_worker_lists_live_capabilities() {
        let live = live_workers(vec![connected("w1")]);
        let info = team_worker_live(&live, "w1").expect("connected worker has live details");
        let json = serde_json::to_value(&info).expect("serializes");
        assert_eq!(json["architectures"], serde_json::json!(["x86_64-linux"]));
        assert_eq!(json["system_features"], serde_json::json!(["kvm"]));
        assert_eq!(json["max_concurrent_builds"], 4);
        assert_eq!(json["ram_total_mb"], 16384);
    }

    #[test]
    fn disconnected_team_worker_has_no_live() {
        let live = live_workers(vec![connected("w1")]);
        assert!(team_worker_live(&live, "w2").is_none());
    }
}
