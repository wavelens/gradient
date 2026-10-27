/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::has_permission;
use crate::api_spec::ProjectPath;
use crate::resource::Resource;
use anyhow::Context;
use axum::extract::State;
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_entity::worker_registration::{
    ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration, Model as MWorkerRegistration,
};
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::{ConnectionFailure, ConnectionFailures};
use gradient_types::ids::*;
use gradient_types::permissions::Permission;
use gradient_types::{BaseResponse, MTeamWorker};
use gradient_wire::types::GradientCapabilities;
use sea_orm::ActiveValue::Set;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::refusal::ProjectRefusal;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::scope::{Member, ProjectScope};

fn default_true() -> bool {
    true
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RegisterWorkerRequest {
    pub worker_id: String,
    pub url: Option<String>,
    pub display_name: String,
    pub token: Option<String>,
    #[serde(default = "default_true")]
    pub enable_network: bool,
    #[serde(default = "default_true")]
    pub enable_secrets: bool,
    #[serde(default)]
    pub enable_exec: bool,
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    #[serde(default = "default_true")]
    pub enable_build: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegisterWorkerResponse {
    pub peer_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_by: Option<UserId>,
    pub enable_network: bool,
    pub enable_secrets: bool,
    pub enable_exec: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub gradient_ci: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
}

impl ProjectWorkerEntry {
    fn of_registration(
        registration: MWorkerRegistration,
        connection: WorkerConnection,
        live: Option<WorkerLiveInfo>,
    ) -> Self {
        Self {
            worker_id: registration.worker_id,
            display_name: registration.display_name,
            registered_at: registration.created_at,
            active: registration.active,
            url: registration.url,
            created_by: registration.created_by,
            enable_network: registration.enable_network,
            enable_secrets: registration.enable_secrets,
            enable_exec: registration.enable_exec,
            enable_eval: registration.enable_eval,
            enable_build: registration.enable_build,
            team: None,
            gradient_ci: registration.gradient_ci,
            connection,
            live,
        }
    }

    fn of_team_worker(
        team: String,
        worker: MTeamWorker,
        connection: WorkerConnection,
        live: Option<WorkerLiveInfo>,
    ) -> Self {
        Self {
            worker_id: worker.worker_id,
            display_name: worker.display_name,
            registered_at: worker.created_at,
            active: worker.active,
            url: worker.url,
            created_by: worker.created_by,
            enable_network: worker.enable_network,
            enable_secrets: worker.enable_secrets,
            enable_exec: worker.enable_exec,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
            team: Some(team),
            gradient_ci: worker.gradient_ci,
            connection,
            live,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerConnection {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ConnectionFailure>,
}

pub(crate) fn worker_connection(
    live: &std::collections::HashMap<String, WorkerInfo>,
    failures: Option<&ConnectionFailures>,
    worker_id: &str,
) -> WorkerConnection {
    WorkerConnection {
        connected: live.contains_key(worker_id),
        last_error: failures.and_then(|f| f.last(worker_id)),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatchWorkerRequest {
    pub active: Option<bool>,
    pub display_name: Option<String>,
    pub enable_network: Option<bool>,
    pub enable_secrets: Option<bool>,
    pub enable_exec: Option<bool>,
    pub enable_eval: Option<bool>,
    pub enable_build: Option<bool>,
}

pub(crate) fn patch_changes_capabilities(body: &PatchWorkerRequest) -> bool {
    [
        body.enable_network,
        body.enable_secrets,
        body.enable_exec,
        body.enable_eval,
        body.enable_build,
    ]
    .iter()
    .any(Option::is_some)
}

pub(crate) fn patch_edits_managed_fields(body: &PatchWorkerRequest) -> bool {
    body.display_name.is_some() || patch_changes_capabilities(body)
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerLiveInfo {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_total_mb: Option<u64>,
}

impl From<&WorkerInfo> for WorkerLiveInfo {
    fn from(w: &WorkerInfo) -> Self {
        Self {
            capabilities: w.capabilities.clone(),
            architectures: w.architectures.clone(),
            system_features: w.system_features.clone(),
            max_concurrent_builds: w.max_concurrent_builds,
            assigned_job_count: w.assigned_job_count,
            draining: w.draining,
            ram_total_mb: (w.ram_total_mb > 0).then_some(w.ram_total_mb),
        }
    }
}

pub(crate) fn encrypt_token(crypt_file: &str, token: &str) -> WebResult<String> {
    gradient_sources::encrypt_secret(crypt_file, token)
        .context("encrypt the worker token")
        .map_err(Into::into)
}

pub(crate) fn encrypt_for_dialing(
    crypt_file: &str,
    url: Option<&str>,
    token: &str,
) -> WebResult<Option<String>> {
    url.filter(|u| !u.trim().is_empty())
        .map(|_| encrypt_token(crypt_file, token))
        .transpose()
}

pub(crate) async fn reauth_project_workers(db: &WebDb, scheduler: &Scheduler, project: ProjectId) {
    match gradient_db::projects::workers::worker_ids_for_project(db, project).await {
        Ok(workers) => {
            for worker_id in &workers {
                scheduler.request_reauth(worker_id).await;
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, project_id = %project, "failed to reauth the project's workers");
        }
    }
}

#[utoipa::path(
    post,
    path = "/projects/{project}/workers",
    operation_id = "registerProjectWorker",
    tag = "workers",
    summary = "Register a worker under a project",
    description = "Registers a worker ID under this project as a peer. Returns a one-time\nplaintext token that the worker operator must add to their worker config.\nThe token is shown only once - it cannot be retrieved later.",
    params(ProjectPath),
    request_body = RegisterWorkerRequest,
    responses((status = OK, description = "Worker registered", body = RegisterWorkerResponse), WebError)
)]
pub async fn post_project_worker(
    ProjectScope { project, user, .. }: ProjectScope<Member>,
    state: State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisterWorkerResponse>>> {
    let worker_uuid =
        Uuid::parse_str(&body.worker_id).map_err(|_| SharedRefusal::InvalidWorkerId)?;
    let worker_id_str = worker_uuid.to_string();
    if gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id_str).await? {
        return Err(ProjectRefusal::WorkerIdBelongsToTeamWorker.into());
    }
    if body.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
        ensure_dialable_worker_id(&state.web_db, &worker_id_str).await?;
    }

    let (token, return_token) = crate::endpoints::worker_tokens::issue(body.token)?;

    let token_hash = password_auth::generate_hash(&token);
    let token_encrypted = encrypt_for_dialing(
        &state.config.secrets.crypt_file,
        body.url.as_deref(),
        &token,
    )?;

    gradient_db::rows::insert::<EWorkerRegistration, _>(
        &state.web_db,
        MWorkerRegistration {
            id: WorkerRegistrationId::now_v7(),
            peer_id: project.id,
            worker_id: worker_id_str.clone(),
            token_hash,
            token_encrypted,
            url: body.url,
            display_name: body.display_name.trim().to_string(),
            active: true,
            enable_network: body.enable_network,
            enable_secrets: body.enable_secrets,
            enable_exec: body.enable_exec,
            enable_eval: body.enable_eval,
            enable_build: body.enable_build,
            created_by: Some(user.id),
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_unique_violation(e, ProjectRefusal::WorkerIdRegistered))?;

    scheduler.request_reauth(&worker_id_str).await;

    if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project.id).await {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after worker registration",
        );
    }

    Ok(ok_json(RegisterWorkerResponse {
        peer_id: project.id,
        token: if return_token { Some(token) } else { None },
    }))
}

async fn ensure_dialable_worker_id(db: &WebDb, worker_id: &str) -> WebResult<()> {
    let team_worker = gradient_db::teams::workers::is_team_worker(db, worker_id).await?;

    let gradient_ci =
        gradient_db::worker_registrations::is_gradient_ci_worker(db, worker_id).await?;
    if team_worker || gradient_ci {
        return Err(ProjectRefusal::WorkerIdBelongsToOtherConnection.into());
    }

    Ok(())
}

/// Open-mode workers (`authorized_peers == None`) are matching any project. Restricted workers are
/// matching only the projects whose token they presented in the handshake.
fn worker_live_for_project(info: &WorkerInfo, project: ProjectId) -> bool {
    info.authorized_peers
        .as_ref()
        .is_none_or(|peers| peers.contains(&project))
}

pub(crate) fn live_workers(
    workers: Vec<WorkerInfo>,
) -> std::collections::HashMap<String, WorkerInfo> {
    workers.into_iter().map(|w| (w.id.clone(), w)).collect()
}

#[utoipa::path(
    get,
    path = "/projects/{project}/workers",
    operation_id = "listProjectWorkers",
    tag = "workers",
    summary = "List registered workers for a project",
    description = "Returns all workers registered under this project (peer), merged with\nlive status from the scheduler for currently connected workers, plus the\nworkers of every team the project grants its workers (`team` set, read-only here).\nLive fields (`architectures`, `system_features`, etc.) are only populated\nfor build-capable workers that have sent `WorkerCapabilities`.",
    params(ProjectPath),
    responses((status = OK, description = "Worker list", body = Vec<ProjectWorkerEntry>), WebError)
)]
pub async fn get_project_workers(
    ProjectScope {
        project,
        user,
        api_key,
        db,
        ..
    }: ProjectScope<Member>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<ProjectWorkerEntry>>>> {
    let manages_workers = user.superuser
        || has_permission(
            &db.web_db,
            user.id,
            project.id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?;

    let failures = manages_workers.then_some(&*scheduler.connection_failures);

    let registrations =
        gradient_db::worker_registrations::of_project(&db.web_db, project.id).await?;

    let live_workers = live_workers(scheduler.workers_info().await);

    let live_for = |worker_id: &str| {
        live_workers
            .get(worker_id)
            .filter(|w| worker_live_for_project(w, project.id))
            .map(WorkerLiveInfo::from)
    };

    let mut entries: Vec<ProjectWorkerEntry> = registrations
        .into_iter()
        .map(|reg| {
            let live = live_for(&reg.worker_id);
            let connection = worker_connection(&live_workers, failures, &reg.worker_id);
            ProjectWorkerEntry::of_registration(reg, connection, live)
        })
        .collect();

    let team_workers =
        gradient_db::teams::workers::team_workers_for_project(&db.web_db, project.id).await?;
    entries.extend(team_workers.into_iter().map(|(team, worker)| {
        let live = live_for(&worker.worker_id);
        let connection = worker_connection(&live_workers, failures, &worker.worker_id);
        ProjectWorkerEntry::of_team_worker(team, worker, connection, live)
    }));

    Ok(ok_json(entries))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerSamplePoint {
    pub at: NaiveDateTime,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<i64>,
    pub ram_total_mb: Option<i64>,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub assigned_jobs: i32,
    pub max_concurrent_builds: i32,
    pub state: i16,
}

impl From<gradient_entity::worker_sample::Model> for WorkerSamplePoint {
    fn from(sample: gradient_entity::worker_sample::Model) -> Self {
        Self {
            at: sample.at,
            cpu_usage_pct: sample.cpu_usage_pct,
            ram_free_mb: sample.ram_free_mb,
            ram_total_mb: sample.ram_total_mb,
            disk_speed_mbps: sample.disk_speed_mbps,
            upload_speed_mbps: sample.upload_speed_mbps,
            download_speed_mbps: sample.download_speed_mbps,
            assigned_jobs: sample.assigned_jobs,
            max_concurrent_builds: sample.max_concurrent_builds,
            state: i16::from(sample.state),
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerConnectionEntry {
    pub connected_at: NaiveDateTime,
    pub disconnected_at: Option<NaiveDateTime>,
}

impl From<gradient_entity::worker_connection::Model> for WorkerConnectionEntry {
    fn from(connection: gradient_entity::worker_connection::Model) -> Self {
        Self {
            connected_at: connection.connected_at,
            disconnected_at: connection.disconnected_at,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerMetricsResponse {
    pub worker_id: String,
    pub display_name: String,
    pub samples: Vec<WorkerSamplePoint>,
    pub connections: Vec<WorkerConnectionEntry>,
    pub jobs_dispatched: u64,
}

#[utoipa::path(
    get,
    path = "/projects/{project}/workers/{worker_id}/metrics",
    operation_id = "getProjectWorkerMetrics",
    tag = "board",
    summary = "Per-worker metrics",
    description = "Live-metric sample time-series (CPU, RAM, disk, upload, download, load), connect/disconnect history, and total assigned-job count for one worker. Project members only, and only for a worker registered in the project or enabled for it as a base worker; the telemetry covers everything the worker did, not only this project's jobs.",
    params(ProjectPath, ("worker_id" = String, Path)),
    responses((status = OK, description = "Worker statistics", body = WorkerMetricsResponse), WebError)
)]
pub async fn get_project_worker_metrics(
    ProjectScope {
        project, params, ..
    }: ProjectScope<Member>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<WorkerMetricsResponse>>> {
    let worker_id: String = params.param("worker_id")?;

    let registration_name = gradient_db::worker_registrations::by_project_and_worker_id(
        &state.web_db,
        project.id,
        &worker_id,
    )
    .await?
    .map(|r| r.display_name);
    let team_name = match registration_name {
        Some(_) => None,
        None => gradient_db::teams::workers::team_workers_for_project(&state.web_db, project.id)
            .await?
            .into_iter()
            .find(|(_, worker)| worker.worker_id == worker_id)
            .map(|(_, worker)| worker.display_name),
    };
    let display_name = registration_name
        .or(team_name)
        .ok_or_else(|| WebError::not_found(Resource::Worker))?;

    let samples =
        gradient_db::worker_registrations::oldest_samples(&state.web_db, &worker_id, 2000)
            .await?
            .into_iter()
            .map(WorkerSamplePoint::from)
            .collect();

    let connections =
        gradient_db::worker_registrations::latest_connections(&state.web_db, &worker_id, 100)
            .await?
            .into_iter()
            .map(WorkerConnectionEntry::from)
            .collect();

    let jobs_dispatched = gradient_db::worker_registrations::dispatched_job_count(
        &state.web_db,
        &worker_id,
        project.id,
    )
    .await?;

    Ok(ok_json(WorkerMetricsResponse {
        display_name,
        worker_id,
        samples,
        connections,
        jobs_dispatched,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WorkerTestResponse {
    pub ok: bool,
    pub connected: bool,
    pub authorized_for_project: bool,
    pub message: String,
}

fn worker_test_result(connected: bool, authorized_for_project: bool) -> (bool, String) {
    if !connected {
        (false, "worker is not connected".to_string())
    } else if !authorized_for_project {
        (
            false,
            "worker is connected but not authorized for this project".to_string(),
        )
    } else {
        (true, "worker is connected and authorized".to_string())
    }
}

#[utoipa::path(
    post,
    path = "/projects/{project}/workers/{worker_id}/test",
    operation_id = "testProjectWorker",
    tag = "workers",
    summary = "Test worker connectivity and authorization",
    description = "Checks whether the worker is currently connected and whether it has\nsuccessfully authorized for the given project. Useful for\nverifying worker setup without waiting for a real job.",
    params(ProjectPath, ("worker_id" = String, Path)),
    responses((status = OK, description = "Test result", body = WorkerTestResponse), WebError)
)]
pub async fn post_project_worker_test(
    ProjectScope {
        project, params, ..
    }: ProjectScope<Member>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<WorkerTestResponse>>> {
    let worker_id: String = params.param("worker_id")?;

    let connected = scheduler.is_worker_connected(&worker_id).await;
    let authorized_for_project = scheduler
        .worker_authorized_for_project(&worker_id, project.id)
        .await;
    let (ok, message) = worker_test_result(connected, authorized_for_project);

    Ok(ok_json(WorkerTestResponse {
        ok,
        connected,
        authorized_for_project,
        message,
    }))
}

#[utoipa::path(
    patch,
    path = "/projects/{project}/workers/{worker_id}",
    operation_id = "patchProjectWorker",
    tag = "workers",
    summary = "Activate or deactivate a worker registration",
    description = "Sets the `active` flag on a worker registration. Deactivating a worker\ntriggers an immediate re-auth request, causing the worker to be disconnected\nif it is currently connected. Active jobs may be aborted.\nA state-managed worker accepts only `active` and answers `409` for other fields.\nThe next server start restores the declared value. A team worker answers `409`;\nit is changed on its team.",
    params(ProjectPath, ("worker_id" = String, Path)),
    request_body = PatchWorkerRequest,
    responses((status = OK, description = "Worker updated", body = String, content_type = "application/json"), WebError)
)]
pub async fn patch_project_worker(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Member>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    if gradient_db::teams::workers::is_team_worker(&db.web_db, &worker_id).await? {
        return Err(ProjectRefusal::TeamWorkerManagedOnTeamPage.into());
    }

    let reg = gradient_db::worker_registrations::by_project_and_worker_id(
        &db.web_db, project.id, &worker_id,
    )
    .await?
    .or_not_found(Resource::WorkerRegistration)?;

    if reg.managed && patch_edits_managed_fields(&body) {
        return Err(SharedRefusal::WorkerManagedByState.into());
    }

    let mut active_model: AWorkerRegistration = reg.into();

    if let Some(active) = body.active {
        active_model.active = Set(active);
    }
    if let Some(ref name) = body.display_name {
        active_model.display_name = Set(name.trim().to_string());
    }
    let caps_changed = patch_changes_capabilities(&body);
    if let Some(v) = body.enable_network {
        active_model.enable_network = Set(v);
    }
    if let Some(v) = body.enable_secrets {
        active_model.enable_secrets = Set(v);
    }
    if let Some(v) = body.enable_exec {
        active_model.enable_exec = Set(v);
    }
    if let Some(v) = body.enable_eval {
        active_model.enable_eval = Set(v);
    }
    if let Some(v) = body.enable_build {
        active_model.enable_build = Set(v);
    }
    gradient_db::rows::update::<EWorkerRegistration, _>(&db.web_db, active_model).await?;

    if let Some(false) = body.active {
        let project_set = std::collections::HashSet::from([project.id]);
        scheduler
            .abort_project_jobs_on_worker(&worker_id, &project_set)
            .await;
    }

    if body.active.is_some() || caps_changed {
        scheduler.request_reauth(&worker_id).await;
    }

    if (matches!(body.active, Some(true)) || matches!(body.enable_eval, Some(true)))
        && let Err(e) = gradient_ci::unpark_no_workers_for_project(&db.web_db, project.id).await
    {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after worker patch",
        );
    }

    Ok(ok_json(format!("worker '{}' updated", worker_id)))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/workers/{worker_id}",
    operation_id = "deleteProjectWorker",
    tag = "workers",
    summary = "Unregister a worker from a project",
    description = "Removes the worker registration. The worker may still be connected until\nit disconnects naturally - it will not be able to reconnect after that.\nA team worker answers `409`; it is deleted on its team.",
    params(ProjectPath, ("worker_id" = String, Path)),
    responses((status = OK, description = "Worker unregistered", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_project_worker(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Member>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    let deleted = gradient_db::worker_registrations::delete_by_project_and_worker_id(
        &db.web_db, project.id, &worker_id,
    )
    .await?;

    if deleted == 0 {
        if gradient_db::teams::workers::is_team_worker(&db.web_db, &worker_id).await? {
            return Err(ProjectRefusal::TeamWorkerManagedOnTeamPage.into());
        }

        return Err(WebError::not_found(Resource::WorkerRegistration));
    }

    let project_set = std::collections::HashSet::from([project.id]);
    scheduler
        .abort_project_jobs_on_worker(&worker_id, &project_set)
        .await;

    scheduler.request_reauth(&worker_id).await;
    scheduler.connection_failures.clear(&worker_id);

    Ok(ok_json(format!("worker '{}' unregistered", worker_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_scheduler::connection_failures::ConnectionDirection;
    use std::collections::HashSet;

    fn worker(authorized: Option<Vec<ProjectId>>) -> WorkerInfo {
        WorkerInfo {
            id: "w1".into(),
            capabilities: GradientCapabilities::default(),
            architectures: vec![],
            system_features: vec![],
            max_concurrent_builds: 1,
            assigned_job_count: 0,
            assigned_build_count: 0,
            draining: false,
            authorized_peers: authorized.map(|v| v.into_iter().collect::<HashSet<_>>()),
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
            exec: None,
        }
    }

    #[test]
    fn a_dialed_registration_keeps_its_token_readable_for_the_server() {
        let crypt = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(crypt.path(), "a-32-byte-crypt-key-for-the-test").unwrap();
        let path = crypt.path().to_string_lossy().into_owned();

        assert_eq!(encrypt_for_dialing(&path, None, "t1").unwrap(), None);
        let encrypted = encrypt_for_dialing(&path, Some("wss://w.example/proto"), "t1")
            .unwrap()
            .expect("encrypted");
        assert_eq!(
            gradient_sources::decrypt_secret(&path, &encrypted).unwrap(),
            "t1"
        );
    }

    #[test]
    fn an_offline_worker_carries_its_last_failure() {
        let failures = ConnectionFailures::default();
        failures.record(
            "w1",
            ConnectionDirection::Outbound,
            false,
            "dial timed out after 10 s",
        );

        let connection =
            worker_connection(&std::collections::HashMap::new(), Some(&failures), "w1");

        assert!(!connection.connected);
        assert_eq!(
            connection.last_error.map(|e| e.reason),
            Some("dial timed out after 10 s".to_string())
        );
    }

    #[test]
    fn the_entry_flattens_its_connection_state() {
        let entry = ProjectWorkerEntry {
            worker_id: "w1".into(),
            display_name: "Worker 1".into(),
            registered_at: gradient_types::now(),
            active: true,
            url: None,
            created_by: None,
            enable_network: true,
            enable_secrets: true,
            enable_exec: true,
            enable_eval: true,
            enable_build: true,
            team: None,
            gradient_ci: false,
            connection: WorkerConnection {
                connected: false,
                last_error: None,
            },
            live: None,
        };

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["connected"], false);
        assert_eq!(json["gradient_ci"], false);
        assert!(json.get("last_error").is_none());
        assert!(json.get("team").is_none());
    }

    #[test]
    fn restricted_worker_is_live_only_on_authorized_projects() {
        let project_a = ProjectId::now_v7();
        let project_b = ProjectId::now_v7();
        let w = worker(Some(vec![project_a]));
        assert!(worker_live_for_project(&w, project_a));
        assert!(!worker_live_for_project(&w, project_b));
    }

    #[test]
    fn open_worker_is_live_on_any_project() {
        let w = worker(None);
        assert!(worker_live_for_project(&w, ProjectId::now_v7()));
    }

    fn empty_patch() -> PatchWorkerRequest {
        PatchWorkerRequest {
            active: None,
            display_name: None,
            enable_network: None,
            enable_secrets: None,
            enable_exec: None,
            enable_eval: None,
            enable_build: None,
        }
    }

    #[test]
    fn active_only_patch_is_allowed_on_managed_worker() {
        let body = PatchWorkerRequest {
            active: Some(true),
            ..empty_patch()
        };
        assert!(!patch_edits_managed_fields(&body));
        assert!(!patch_edits_managed_fields(&empty_patch()));
    }

    #[test]
    fn editing_name_or_caps_is_rejected_on_managed_worker() {
        for body in [
            PatchWorkerRequest {
                display_name: Some("x".into()),
                ..empty_patch()
            },
            PatchWorkerRequest {
                enable_network: Some(false),
                ..empty_patch()
            },
            PatchWorkerRequest {
                enable_secrets: Some(false),
                ..empty_patch()
            },
            PatchWorkerRequest {
                enable_exec: Some(false),
                ..empty_patch()
            },
            PatchWorkerRequest {
                enable_eval: Some(true),
                ..empty_patch()
            },
            PatchWorkerRequest {
                enable_build: Some(false),
                ..empty_patch()
            },
        ] {
            assert!(patch_edits_managed_fields(&body));
        }
    }

    #[test]
    fn patching_secrets_changes_the_capabilities() {
        let body = PatchWorkerRequest {
            enable_secrets: Some(false),
            ..empty_patch()
        };
        assert!(patch_changes_capabilities(&body));
        assert!(!patch_changes_capabilities(&PatchWorkerRequest {
            active: Some(false),
            display_name: Some("x".into()),
            ..empty_patch()
        }));
    }

    #[test]
    fn worker_test_result_covers_all_states() {
        let (ok, msg) = worker_test_result(false, false);
        assert!(!ok);
        assert_eq!(msg, "worker is not connected");

        let (ok, msg) = worker_test_result(false, true);
        assert!(!ok);
        assert_eq!(msg, "worker is not connected");

        let (ok, msg) = worker_test_result(true, false);
        assert!(!ok);
        assert_eq!(
            msg,
            "worker is connected but not authorized for this project"
        );

        let (ok, msg) = worker_test_result(true, true);
        assert!(ok);
        assert_eq!(msg, "worker is connected and authorized");
    }
}
