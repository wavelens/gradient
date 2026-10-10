/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, has_permission, load_project};
use crate::authorization::MaybeApiKey;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::permissions::Permission;
use gradient_entity::worker_registration::{
    self, ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration,
    Model as MWorkerRegistration,
};
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::{ConnectionFailure, ConnectionFailures};
use gradient_types::ids::*;
use gradient_types::{BaseResponse, MUser};
use gradient_wire::types::GradientCapabilities;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct RegisterWorkerRequest {
    pub worker_id: String,
    pub url: Option<String>,
    pub display_name: String,
    pub token: Option<String>,
    #[serde(default = "default_true")]
    pub enable_fetch: bool,
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    #[serde(default = "default_true")]
    pub enable_build: bool,
}

#[derive(Serialize)]
pub struct RegisterWorkerResponse {
    pub peer_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct ProjectWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_by: Option<UserId>,
    pub enable_fetch: bool,
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

#[derive(Serialize)]
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

#[derive(Deserialize)]
pub struct PatchWorkerRequest {
    pub active: Option<bool>,
    pub display_name: Option<String>,
    pub enable_fetch: Option<bool>,
    pub enable_eval: Option<bool>,
    pub enable_build: Option<bool>,
}

pub(crate) fn patch_edits_managed_fields(body: &PatchWorkerRequest) -> bool {
    body.display_name.is_some()
        || body.enable_fetch.is_some()
        || body.enable_eval.is_some()
        || body.enable_build.is_some()
}

#[derive(Serialize)]
pub struct WorkerLiveInfo {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
}

pub(crate) fn encrypt_token(crypt_file: &str, token: &str) -> WebResult<String> {
    gradient_sources::encrypt_secret(crypt_file, token)
        .map_err(|e| WebError::internal(format!("encrypting the worker token failed: {e}")))
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

pub(crate) async fn reauth_project_workers(
    state: &ServerState,
    scheduler: &Scheduler,
    project: ProjectId,
) {
    match gradient_db::projects::workers::worker_ids_for_project(&state.web_db, project).await {
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

pub async fn post_project_worker(
    state: State<Arc<ServerState>>,
    Path(project): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisterWorkerResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let worker_uuid = Uuid::parse_str(&body.worker_id)
        .map_err(|_| WebError::bad_request("worker_id must be a valid UUID"))?;
    let worker_id_str = worker_uuid.to_string();
    if gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id_str).await? {
        return Err(WebError::conflict("the worker id belongs to a team worker"));
    }
    if body.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
        ensure_dialable_worker_id(&state, &worker_id_str).await?;
    }

    let (token, return_token) = crate::endpoints::worker_tokens::issue(body.token)?;

    let token_hash = password_auth::generate_hash(&token);
    let token_encrypted = encrypt_for_dialing(
        &state.config.secrets.crypt_file,
        body.url.as_deref(),
        &token,
    )?;

    let row = MWorkerRegistration {
        id: WorkerRegistrationId::now_v7(),
        peer_id: project.id,
        worker_id: worker_id_str.clone(),
        token_hash,
        token_encrypted,
        url: body.url,
        display_name: body.display_name.trim().to_string(),
        active: true,
        enable_fetch: body.enable_fetch,
        enable_eval: body.enable_eval,
        enable_build: body.enable_build,
        created_by: Some(user.id),
        created_at: gradient_types::now(),
        ..Default::default()
    }
    .into_active_model();

    row.insert(&state.web_db).await?;

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

async fn ensure_dialable_worker_id(state: &ServerState, worker_id: &str) -> WebResult<()> {
    let team_worker = gradient_db::teams::workers::is_team_worker(&state.web_db, worker_id).await?;

    let gradient_ci = EWorkerRegistration::find()
        .filter(worker_registration::Column::WorkerId.eq(worker_id))
        .filter(worker_registration::Column::GradientCi.eq(true))
        .one(&state.web_db)
        .await?
        .is_some();
    if team_worker || gradient_ci {
        return Err(WebError::conflict(
            "the worker id belongs to a team worker or a Gradient.CI connection",
        ));
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

pub async fn get_project_workers(
    state: State<Arc<ServerState>>,
    Path(project): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<ProjectWorkerEntry>>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;
    let manages_workers = user.superuser
        || has_permission(
            &state,
            user.id,
            project.id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?;

    let failures = manages_workers.then_some(&*scheduler.connection_failures);

    let registrations = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(project.id))
        .all(&state.web_db)
        .await?;

    let live_workers = live_workers(scheduler.workers_info().await);

    let live_for = |worker_id: &str| {
        live_workers
            .get(worker_id)
            .filter(|w| worker_live_for_project(w, project.id))
            .map(|w| WorkerLiveInfo {
                capabilities: w.capabilities.clone(),
                architectures: w.architectures.clone(),
                system_features: w.system_features.clone(),
                max_concurrent_builds: w.max_concurrent_builds,
                assigned_job_count: w.assigned_job_count,
                draining: w.draining,
            })
    };

    let mut entries: Vec<ProjectWorkerEntry> = registrations
        .into_iter()
        .map(|reg| {
            let live = live_for(&reg.worker_id);
            let connection = worker_connection(&live_workers, failures, &reg.worker_id);
            ProjectWorkerEntry {
                gradient_ci: reg.gradient_ci,
                connection,
                worker_id: reg.worker_id,
                display_name: reg.display_name,
                registered_at: reg.created_at,
                active: reg.active,
                url: reg.url,
                created_by: reg.created_by,
                enable_fetch: reg.enable_fetch,
                enable_eval: reg.enable_eval,
                enable_build: reg.enable_build,
                team: None,
                live,
            }
        })
        .collect();

    let team_workers =
        gradient_db::teams::workers::team_workers_for_project(&state.web_db, project.id).await?;
    entries.extend(team_workers.into_iter().map(|(team, worker)| {
        let live = live_for(&worker.worker_id);
        let connection = worker_connection(&live_workers, failures, &worker.worker_id);
        ProjectWorkerEntry {
            worker_id: worker.worker_id,
            display_name: worker.display_name,
            registered_at: worker.created_at,
            active: worker.active,
            url: worker.url,
            created_by: worker.created_by,
            enable_fetch: worker.enable_fetch,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
            team: Some(team),
            gradient_ci: worker.gradient_ci,
            connection,
            live,
        }
    }));

    Ok(ok_json(entries))
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct WorkerConnectionEntry {
    pub connected_at: NaiveDateTime,
    pub disconnected_at: Option<NaiveDateTime>,
}

#[derive(Serialize)]
pub struct WorkerMetricsResponse {
    pub worker_id: String,
    pub display_name: String,
    pub samples: Vec<WorkerSamplePoint>,
    pub connections: Vec<WorkerConnectionEntry>,
    pub jobs_dispatched: u64,
}

pub async fn get_project_worker_metrics(
    state: State<Arc<ServerState>>,
    Path((project, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
) -> WebResult<Json<BaseResponse<WorkerMetricsResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let registration_name = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(project.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .one(&state.web_db)
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
        .ok_or_else(|| WebError::not_found("worker"))?;

    let samples = gradient_entity::worker_sample::Entity::find()
        .filter(gradient_entity::worker_sample::Column::WorkerId.eq(&worker_id))
        .order_by_asc(gradient_entity::worker_sample::Column::At)
        .limit(2000)
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|s| WorkerSamplePoint {
            at: s.at,
            cpu_usage_pct: s.cpu_usage_pct,
            ram_free_mb: s.ram_free_mb,
            ram_total_mb: s.ram_total_mb,
            disk_speed_mbps: s.disk_speed_mbps,
            upload_speed_mbps: s.upload_speed_mbps,
            download_speed_mbps: s.download_speed_mbps,
            assigned_jobs: s.assigned_jobs,
            max_concurrent_builds: s.max_concurrent_builds,
            state: i16::from(s.state),
        })
        .collect();

    let connections = gradient_entity::worker_connection::Entity::find()
        .filter(gradient_entity::worker_connection::Column::WorkerId.eq(&worker_id))
        .order_by_desc(gradient_entity::worker_connection::Column::ConnectedAt)
        .limit(100)
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|c| WorkerConnectionEntry {
            connected_at: c.connected_at,
            disconnected_at: c.disconnected_at,
        })
        .collect();

    let jobs_dispatched = gradient_entity::dispatched_job::Entity::find()
        .filter(gradient_entity::dispatched_job::Column::WorkerId.eq(&worker_id))
        .filter(gradient_entity::dispatched_job::Column::Project.eq(project.id))
        .count(&state.web_db)
        .await?;

    Ok(ok_json(WorkerMetricsResponse {
        display_name,
        worker_id,
        samples,
        connections,
        jobs_dispatched,
    }))
}

#[derive(Debug, Serialize)]
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

pub async fn post_project_worker_test(
    state: State<Arc<ServerState>>,
    Path((project, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<WorkerTestResponse>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

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

pub async fn patch_project_worker(
    state: State<Arc<ServerState>>,
    Path((project, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    if gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id).await? {
        return Err(WebError::conflict(
            "team workers are managed on the team page",
        ));
    }

    let reg = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(project.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .one(&state.web_db)
        .await?
        .or_not_found("worker registration")?;

    if reg.managed && patch_edits_managed_fields(&body) {
        return Err(WebError::conflict("worker is managed by server state"));
    }

    let mut active_model: AWorkerRegistration = reg.into();

    if let Some(active) = body.active {
        active_model.active = Set(active);
    }
    if let Some(ref name) = body.display_name {
        active_model.display_name = Set(name.trim().to_string());
    }
    let caps_changed =
        body.enable_fetch.is_some() || body.enable_eval.is_some() || body.enable_build.is_some();
    if let Some(v) = body.enable_fetch {
        active_model.enable_fetch = Set(v);
    }
    if let Some(v) = body.enable_eval {
        active_model.enable_eval = Set(v);
    }
    if let Some(v) = body.enable_build {
        active_model.enable_build = Set(v);
    }
    active_model.update(&state.web_db).await?;

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
        && let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project.id).await
    {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after worker patch",
        );
    }

    Ok(ok_json(format!("worker '{}' updated", worker_id)))
}

pub async fn delete_project_worker(
    state: State<Arc<ServerState>>,
    Path((project, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let project = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        ProjectAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let result = EWorkerRegistration::delete_many()
        .filter(worker_registration::Column::PeerId.eq(project.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .exec(&state.web_db)
        .await?;

    if result.rows_affected == 0 {
        if gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id).await? {
            return Err(WebError::conflict(
                "team workers are managed on the team page",
            ));
        }

        return Err(WebError::not_found("worker registration"));
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
            enable_fetch: true,
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
            enable_fetch: None,
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
                enable_fetch: Some(false),
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
