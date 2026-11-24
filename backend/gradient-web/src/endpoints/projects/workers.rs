/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::has_permission;
use crate::api_spec::ProjectPath;
use crate::audit::{Audit, changed_fields};
use crate::extractors::JsonBody;
use crate::resource::Resource;
use anyhow::Context;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_entity::worker_registration::{
    ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration, Model as MWorkerRegistration,
};
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::ConnectionFailures;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::frontend_page_urls;
use gradient_types::ids::*;
use gradient_types::permissions::Permission;
use gradient_types::{BaseResponse, MTeam, MTeamWorker};
use sea_orm::ActiveValue::Set;
use std::sync::Arc;

use super::refusal::ProjectRefusal;
use crate::endpoints::worker_setup::WorkerRegistrationResponse;
use crate::endpoints::worker_setup::make_new_worker_registration_response_with_nixos_config;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::password_hashing_and_wrong_sign_in_attempts::compute_argon2_hash_for_new_password_on_blocking_pool_with_limited_semaphore_permit;
use crate::refusal::SharedRefusal;
use crate::scope::{Member, ProjectScope, Requires, can};
pub use gradient_api_types::{
    PatchWorkerRequest, ProjectWorkerEntry, RegisterWorkerRequest, WorkerConnection,
    WorkerConnectionEntry, WorkerLiveInfo, WorkerMetricsResponse, WorkerSamplePoint,
    WorkerTestResponse,
};

fn project_worker_entry_from_team_worker_row_connection_state_and_live_status(
    team: MTeam,
    worker: MTeamWorker,
    connection: WorkerConnection,
    live: Option<WorkerLiveInfo>,
) -> ProjectWorkerEntry {
    ProjectWorkerEntry {
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
        team: Some(team.name),
        team_display_name: Some(team.display_name),
        gradient_ci: worker.gradient_ci,
        connection,
        live,
    }
}

fn build_project_worker_entry_from_registration_row_connection_state_and_live_worker_process_details(
    registration: MWorkerRegistration,
    connection: WorkerConnection,
    live: Option<WorkerLiveInfo>,
) -> ProjectWorkerEntry {
    ProjectWorkerEntry {
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
        team_display_name: None,
        gradient_ci: registration.gradient_ci,
        connection,
        live,
    }
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
    description = "Register a new worker under this project. A worker ID and a one-time token come from the server. Response data include a NixOS configuration for the worker machine. The token is visible only once.",
    params(ProjectPath),
    request_body = RegisterWorkerRequest,
    responses((status = OK, description = "Worker registered", body = WorkerRegistrationResponse), WebError)
)]
pub async fn post_project_worker(
    ProjectScope { project, user, .. }: ProjectScope<Requires<can::ManageWorkers>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    JsonBody(body): JsonBody<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<WorkerRegistrationResponse>>> {
    if let Some(url) = body.url.as_deref().filter(|u| !u.trim().is_empty()) {
        gradient_util::http_validation::validate_worker_websocket_url(
            url,
            state.config.proto.allow_worker_urls_on_private_networks,
        )
        .map_err(|e| ProjectRefusal::InvalidWorkerUrl {
            reason: e.to_string(),
        })?;
    }

    let token = crate::endpoints::worker_tokens::issue();
    let registration = make_new_worker_registration_response_with_nixos_config(
        &state.config.server.serve_url,
        &body,
        frontend_page_urls::project_workers_page_url(
            &state.config.server.frontend_url,
            &project.name,
        ),
        project.id,
        token.clone(),
    );
    let worker_id_str = registration.id.clone();

    let token_hash =
        compute_argon2_hash_for_new_password_on_blocking_pool_with_limited_semaphore_permit(
            token.clone(),
        )
        .await?;
    let token_encrypted = encrypt_for_dialing(
        &state.config.secrets.crypt_file,
        body.url.as_deref(),
        &token,
    )?;
    let display_name = body.display_name.trim().to_string();

    gradient_db::rows::insert::<EWorkerRegistration, _>(
        &state.web_db,
        MWorkerRegistration {
            id: WorkerRegistrationId::now_v7(),
            peer_id: project.id,
            worker_id: worker_id_str.clone(),
            token_hash,
            token_encrypted,
            url: body.url,
            display_name: display_name.clone(),
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
    .await?;

    scheduler.request_reauth(&worker_id_str).await;

    if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project.id).await {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after worker registration",
        );
    }

    audit
        .write(
            Action::ProjectWorkerRegister,
            EventOwner::project(project.id),
            serde_json::json!({ "worker_id": worker_id_str, "display_name": display_name }),
        )
        .await;
    Ok(ok_json(registration))
}

fn worker_live_for_project(info: &WorkerInfo, project: ProjectId) -> bool {
    info.project_access.contains(&project)
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
    description = "Return all workers registered under this project (peer). Merge live status from the scheduler for workers that are connected now. Return workers for all teams that share workers with this project (with `team` set and read-only here). Set `live` only on connected workers that presented a token for this project during the handshake. Connected workers without `live` accept no jobs for this project until those workers reconnect with a token. Set live fields (`architectures`, `system_features`, and so on) only on buildable workers that sent `WorkerCapabilities`.",
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
            build_project_worker_entry_from_registration_row_connection_state_and_live_worker_process_details(reg, connection, live)
        })
        .collect();

    let team_workers =
        gradient_db::teams::workers::team_workers_for_project(&db.web_db, project.id).await?;
    entries.extend(team_workers.into_iter().map(|(team, worker)| {
        let live = live_for(&worker.worker_id);
        let connection = worker_connection(&live_workers, failures, &worker.worker_id);
        project_worker_entry_from_team_worker_row_connection_state_and_live_status(
            team, worker, connection, live,
        )
    }));

    Ok(ok_json(entries))
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

    let samples = gradient_db::worker_registrations::latest_resource_samples_by_worker_id(
        &state.web_db,
        &worker_id,
        2000,
    )
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
    }: ProjectScope<Requires<can::ManageWorkers>>,
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
    }: ProjectScope<Requires<can::ManageWorkers>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    JsonBody(body): JsonBody<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    if gradient_db::teams::workers::is_team_worker(&db.web_db, &worker_id).await? {
        return Err(ProjectRefusal::TeamWorkerManagedOnTeamPage.into());
    }
    if body.draining.is_some() {
        return Err(ProjectRefusal::ProjectWorkerPageCannotDrainWorkerOnSharedMachine.into());
    }

    let reg = gradient_db::worker_registrations::by_project_and_worker_id(
        &db.web_db, project.id, &worker_id,
    )
    .await?
    .or_not_found(Resource::WorkerRegistration)?;

    if reg.managed && patch_edits_managed_fields(&body) {
        return Err(SharedRefusal::WorkerManagedByState.into());
    }

    let fields = changed_fields([
        ("active", body.active.is_some()),
        ("display_name", body.display_name.is_some()),
        ("enable_network", body.enable_network.is_some()),
        ("enable_secrets", body.enable_secrets.is_some()),
        ("enable_exec", body.enable_exec.is_some()),
        ("enable_eval", body.enable_eval.is_some()),
        ("enable_build", body.enable_build.is_some()),
    ]);
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

    audit
        .write(
            Action::ProjectWorkerUpdate,
            EventOwner::project(project.id),
            serde_json::json!({ "worker_id": worker_id, "fields": fields["fields"] }),
        )
        .await;
    Ok(ok_json(format!("worker '{}' updated", worker_id)))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/workers/{worker_id}",
    operation_id = "deleteProjectWorker",
    tag = "workers",
    summary = "Unregister a worker from a project",
    description = "Removes the worker registration. The worker may still be connected until\nit disconnects naturally - it will not be able to reconnect after that.\nA team worker or a state-managed worker returns `409`. Delete a team worker in its team.",
    params(ProjectPath, ("worker_id" = String, Path)),
    responses((status = OK, description = "Worker unregistered", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_project_worker(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageWorkers>>,
    audit: Audit,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let worker_id: String = params.param("worker_id")?;

    let Some(registration) = gradient_db::worker_registrations::by_project_and_worker_id(
        &db.web_db, project.id, &worker_id,
    )
    .await?
    else {
        if gradient_db::teams::workers::is_team_worker(&db.web_db, &worker_id).await? {
            return Err(ProjectRefusal::TeamWorkerManagedOnTeamPage.into());
        }
        return Err(WebError::not_found(Resource::WorkerRegistration));
    };
    if registration.managed {
        return Err(SharedRefusal::WorkerManagedByState.into());
    }

    gradient_db::worker_registrations::delete_by_project_and_worker_id(
        &db.web_db, project.id, &worker_id,
    )
    .await?;

    scheduler.request_reauth(&worker_id).await;
    scheduler.connection_failures.clear(&worker_id);

    audit
        .write(
            Action::ProjectWorkerRegistrationDelete,
            EventOwner::project(project.id),
            serde_json::json!({ "worker_id": worker_id, "display_name": registration.display_name }),
        )
        .await;
    Ok(ok_json(format!("worker '{}' unregistered", worker_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_pool::ProjectAccess;
    use gradient_scheduler::connection_failures::ConnectionDirection;
    use gradient_wire::types::GradientCapabilities;
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
            project_access: authorized.map_or(ProjectAccess::AllProjects, |projects| {
                ProjectAccess::OnlyListedProjects(projects.into_iter().collect::<HashSet<_>>())
            }),
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
            zone: None,
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
            gradient_sources::decrypt_secret(&path, &encrypted)
                .unwrap()
                .expose(),
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
            team_display_name: None,
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
            draining: None,
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
