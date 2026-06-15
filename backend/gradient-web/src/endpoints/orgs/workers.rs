/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeApiKey;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use base64::Engine as _;
use chrono::NaiveDateTime;
use gradient_entity::worker_registration::{
    self, ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration,
    Model as MWorkerRegistration,
};
use gradient_entity::{base_worker, organization_base_worker};
use gradient_types::{AOrganizationBaseWorker, EBaseWorker, EOrganizationBaseWorker};
use gradient_types::ids::*;
use gradient_types::proto::GradientCapabilities;
use gradient_types::{BaseResponse, MUser};
use gradient_core::ServerState;
use rand::RngExt as _;
use gradient_scheduler::{Scheduler, WorkerInfo};
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
    /// WebSocket URL where the worker listens for incoming server connections.
    /// When set, the server connects outbound to this URL.
    pub url: Option<String>,
    /// Human-readable display name for this worker.
    pub display_name: String,
    /// Pre-generated token (output of `openssl rand -base64 48`, exactly 64 base64 chars).
    /// When provided the server stores its hash and does NOT return the token in the response.
    pub token: Option<String>,
    /// Per-registration server-side gate for `fetch`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_fetch: bool,
    /// Per-registration server-side gate for `eval`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_eval: bool,
    /// Per-registration server-side gate for `build`. Defaults to true.
    #[serde(default = "default_true")]
    pub enable_build: bool,
}

#[derive(Serialize)]
pub struct RegisterWorkerResponse {
    pub peer_id: OrganizationId,
    /// Only present when the token was server-generated (i.e. not supplied in the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct OrgWorkerEntry {
    pub worker_id: String,
    /// Human-readable display name for this worker (empty string if not set).
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    /// WebSocket URL where the worker accepts incoming server connections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// User who registered this worker. NULL for legacy or declarative rows.
    pub created_by: Option<UserId>,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    /// True for server-level base workers, false for per-org registrations.
    pub is_base: bool,
    /// Present when the worker is currently connected to this server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
}

#[derive(Deserialize)]
pub struct PatchWorkerRequest {
    /// When present, update the active flag.
    pub active: Option<bool>,
    /// When present, update the display name. Empty string clears the name.
    pub display_name: Option<String>,
    /// When present, update the per-registration `fetch` gate.
    pub enable_fetch: Option<bool>,
    /// When present, update the per-registration `eval` gate.
    pub enable_eval: Option<bool>,
    /// When present, update the per-registration `build` gate.
    pub enable_build: Option<bool>,
}

/// Base workers are server-managed: the only patch a member may apply is the
/// per-org `active` opt-in/out. Any attempt to edit name or capability gates is
/// a conflict.
fn patch_edits_base_worker_fields(body: &PatchWorkerRequest) -> bool {
    body.display_name.is_some()
        || body.enable_fetch.is_some()
        || body.enable_eval.is_some()
        || body.enable_build.is_some()
}

#[derive(Serialize)]
pub struct WorkerLiveInfo {
    pub capabilities: GradientCapabilities,
    /// Nix system strings (e.g. "x86_64-linux"). Only populated for workers
    /// with the `build` capability negotiated.
    pub architectures: Vec<String>,
    /// Nix system features (e.g. "kvm"). Only populated for build-capable workers.
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub draining: bool,
}

pub async fn post_org_worker(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisterWorkerResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let worker_uuid = Uuid::parse_str(&body.worker_id)
        .ok()
        .filter(|u| u.get_version() == Some(uuid::Version::Random))
        .ok_or_else(|| WebError::bad_request("worker_id must be a valid UUID v4"))?;
    let worker_id_str = worker_uuid.to_string();

    // Resolve token: use caller-supplied one (after validation) or generate a new one.
    let (token, return_token) = if let Some(provided) = body.token {
        let t = provided.trim().to_string();
        // Must be exactly 64 chars of valid standard base64 (openssl rand -base64 48 output).
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&t)
            .map_err(|_| WebError::bad_request("token is not valid base64"))?;
        if decoded.len() != 48 {
            return Err(WebError::bad_request(
                "token must be 48 raw bytes encoded as base64 (openssl rand -base64 48)",
            ));
        }
        (t, false)
    } else {
        // Generate a cryptographically random 48-byte token, base64-encoded.
        // Equivalent to `openssl rand -base64 48` (produces 64 base64 characters).
        let mut raw = [0u8; 48];
        rand::rng().fill(&mut raw);
        (base64::engine::general_purpose::STANDARD.encode(raw), true)
    };

    let token_hash = password_auth::generate_hash(&token);

    let row = MWorkerRegistration {
        id: WorkerRegistrationId::now_v7(),
        peer_id: org.id,
        worker_id: worker_id_str.clone(),
        token_hash,
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

    // Trigger re-auth if the worker is already connected, so it picks up
    // the new peer registration without requiring a reconnect.
    scheduler.request_reauth(&worker_id_str).await;

    // Re-queue any evaluations parked because the org had no eval-capable
    // worker registration. No-op when the new row isn't eval-capable.
    if let Err(e) = gradient_ci::unpark_no_workers_for_org(&state.web_db, org.id).await {
        tracing::warn!(
            error = %e,
            org_id = %org.id,
            "failed to unpark no-workers evaluations after worker registration",
        );
    }

    Ok(ok_json(RegisterWorkerResponse {
        peer_id: org.id,
        token: if return_token { Some(token) } else { None },
    }))
}

/// A connected worker counts as live for `org` only when it authenticated for
/// it: open-mode workers (`authorized_peers == None`) match any org, restricted
/// workers only the orgs whose token they presented in the handshake. This
/// keeps a worker authorized for one org from showing as connected on another.
fn worker_live_for_org(info: &WorkerInfo, org: OrganizationId) -> bool {
    info.authorized_peers
        .as_ref()
        .is_none_or(|peers| peers.contains(&org))
}

/// Maps an enabled base-worker row into an `OrgWorkerEntry`. `active` reflects
/// whether the requesting org has opted in via `organization_base_worker`.
fn base_worker_entry(
    bw: base_worker::Model,
    active: bool,
    live: Option<WorkerLiveInfo>,
) -> OrgWorkerEntry {
    OrgWorkerEntry {
        active,
        worker_id: bw.worker_id,
        display_name: bw.display_name,
        registered_at: bw.created_at,
        url: bw.url,
        created_by: bw.created_by,
        enable_fetch: bw.enable_fetch,
        enable_eval: bw.enable_eval,
        enable_build: bw.enable_build,
        is_base: true,
        live,
    }
}

pub async fn get_org_workers(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<OrgWorkerEntry>>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    let registrations = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .all(&state.web_db)
        .await?;

    // Build a map of worker_id → live info from the scheduler.
    let live_workers: std::collections::HashMap<String, WorkerInfo> = scheduler
        .workers_info()
        .await
        .into_iter()
        .map(|w| (w.id.clone(), w))
        .collect();

    // Live info is only exposed when the worker has actually authenticated for
    // THIS org. A worker may be globally connected (authorized for some other
    // org) without holding a valid token for this org.
    let live_for = |worker_id: &str| {
        live_workers
            .get(worker_id)
            .filter(|w| worker_live_for_org(w, org.id))
            .map(|w| WorkerLiveInfo {
                capabilities: w.capabilities.clone(),
                architectures: w.architectures.clone(),
                system_features: w.system_features.clone(),
                max_concurrent_builds: w.max_concurrent_builds,
                assigned_job_count: w.assigned_job_count,
                draining: w.draining,
            })
    };

    let mut entries: Vec<OrgWorkerEntry> = registrations
        .into_iter()
        .map(|reg| {
            let live = live_for(&reg.worker_id);
            OrgWorkerEntry {
                worker_id: reg.worker_id,
                display_name: reg.display_name,
                registered_at: reg.created_at,
                active: reg.active,
                url: reg.url,
                created_by: reg.created_by,
                enable_fetch: reg.enable_fetch,
                enable_eval: reg.enable_eval,
                enable_build: reg.enable_build,
                is_base: false,
                live,
            }
        })
        .collect();

    let base_workers = EBaseWorker::find()
        .filter(base_worker::Column::Enabled.eq(true))
        .all(&state.web_db)
        .await?;
    let enabled_ids: std::collections::HashSet<BaseWorkerId> = EOrganizationBaseWorker::find()
        .filter(organization_base_worker::Column::Organization.eq(org.id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|r| r.base_worker)
        .collect();

    entries.extend(base_workers.into_iter().map(|bw| {
        let live = live_for(&bw.worker_id);
        let active = enabled_ids.contains(&bw.id);
        base_worker_entry(bw, active, live)
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
    pub network_speed_mbps: Option<f32>,
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
    pub samples: Vec<WorkerSamplePoint>,
    pub connections: Vec<WorkerConnectionEntry>,
    pub jobs_dispatched: u64,
}

/// Full metrics for one worker: the live-metric sample time-series, the
/// connect/disconnect history, and the total dispatched-job count. Scoped to
/// members of the worker's owning org.
pub async fn get_org_worker_metrics(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
) -> WebResult<Json<BaseResponse<WorkerMetricsResponse>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member { reject_managed: false },
    )
    .await?;

    let samples = gradient_entity::worker_sample::Entity::find()
        .filter(gradient_entity::worker_sample::Column::WorkerId.eq(&worker_id))
        .filter(gradient_entity::worker_sample::Column::Organization.eq(org.id))
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
            network_speed_mbps: s.network_speed_mbps,
            assigned_jobs: s.assigned_jobs,
            max_concurrent_builds: s.max_concurrent_builds,
            state: s.state,
        })
        .collect();

    let connections = gradient_entity::worker_connection::Entity::find()
        .filter(gradient_entity::worker_connection::Column::WorkerId.eq(&worker_id))
        .filter(gradient_entity::worker_connection::Column::Organization.eq(org.id))
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
        .filter(gradient_entity::dispatched_job::Column::Organization.eq(org.id))
        .count(&state.web_db)
        .await?;

    Ok(ok_json(WorkerMetricsResponse {
        samples,
        connections,
        jobs_dispatched,
    }))
}

pub async fn patch_org_worker(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    if let Some(bw) =
        gradient_db::base_workers::enabled_base_worker_by_worker_id(&state.web_db, &worker_id)
            .await?
    {
        if patch_edits_base_worker_fields(&body) {
            return Err(WebError::conflict("base workers are managed by server state"));
        }

        use gradient_entity::organization_base_worker::{Column as OBWC, Entity as OBW};

        match body.active {
            Some(true) => {
                let exists = OBW::find()
                    .filter(OBWC::Organization.eq(org.id))
                    .filter(OBWC::BaseWorker.eq(bw.id))
                    .one(&state.web_db)
                    .await?
                    .is_some();
                if !exists {
                    AOrganizationBaseWorker {
                        id: Set(OrganizationBaseWorkerId::now_v7()),
                        organization: Set(org.id),
                        base_worker: Set(bw.id),
                        created_by: Set(Some(user.id)),
                        created_at: Set(gradient_types::now()),
                    }
                    .insert(&state.web_db)
                    .await?;
                }
            }
            Some(false) => {
                OBW::delete_many()
                    .filter(OBWC::Organization.eq(org.id))
                    .filter(OBWC::BaseWorker.eq(bw.id))
                    .exec(&state.web_db)
                    .await?;
                let org_set = std::collections::HashSet::from([org.id]);
                scheduler.abort_org_jobs_on_worker(&worker_id, &org_set).await;
            }
            None => {}
        }

        scheduler.request_reauth(&worker_id).await;
        if matches!(body.active, Some(true)) {
            let _ = gradient_ci::unpark_no_workers_for_org(&state.web_db, org.id).await;
        }

        return Ok(ok_json("ok".to_string()));
    }

    let reg = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .one(&state.web_db)
        .await?
        .or_not_found("worker registration")?;

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

    // When deactivating: abort in-flight jobs from this org on the worker
    // before triggering reauth, so the worker stops them immediately.
    if let Some(false) = body.active {
        let org_set = std::collections::HashSet::from([org.id]);
        scheduler
            .abort_org_jobs_on_worker(&worker_id, &org_set)
            .await;
    }

    // Trigger re-auth so the worker's authorized peer set or negotiated
    // capabilities are updated (or the worker is kicked if all registrations
    // are now inactive).
    if body.active.is_some() || caps_changed {
        scheduler.request_reauth(&worker_id).await;
    }

    // Toggling `active` on or enabling the `eval` capability may newly satisfy
    // the no-workers gate. The unpark is self-guarded against the org still
    // lacking an eval-capable registration, so calling unconditionally here
    // is safe.
    if (matches!(body.active, Some(true)) || matches!(body.enable_eval, Some(true)))
        && let Err(e) = gradient_ci::unpark_no_workers_for_org(&state.web_db, org.id).await
    {
        tracing::warn!(
            error = %e,
            org_id = %org.id,
            "failed to unpark no-workers evaluations after worker patch",
        );
    }

    Ok(ok_json(format!("worker '{}' updated", worker_id)))
}

pub async fn delete_org_worker(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    if gradient_db::base_workers::enabled_base_worker_by_worker_id(&state.web_db, &worker_id)
        .await?
        .is_some()
    {
        return Err(WebError::conflict(
            "base workers cannot be deleted; manage them via server state",
        ));
    }

    let result = EWorkerRegistration::delete_many()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .exec(&state.web_db)
        .await?;

    if result.rows_affected == 0 {
        return Err(WebError::not_found("worker registration"));
    }

    // Abort in-flight jobs from this org on the worker before triggering reauth.
    let org_set = std::collections::HashSet::from([org.id]);
    scheduler
        .abort_org_jobs_on_worker(&worker_id, &org_set)
        .await;

    // Trigger re-auth so the worker loses authorization for the removed peer.
    scheduler.request_reauth(&worker_id).await;

    Ok(ok_json(format!("worker '{}' unregistered", worker_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn worker(authorized: Option<Vec<OrganizationId>>) -> WorkerInfo {
        WorkerInfo {
            id: "w1".into(),
            capabilities: GradientCapabilities::default(),
            architectures: vec![],
            system_features: vec![],
            max_concurrent_builds: 1,
            assigned_job_count: 0,
            draining: false,
            authorized_peers: authorized.map(|v| v.into_iter().collect::<HashSet<_>>()),
            organization: None,
            cpu_usage_pct: 0.0,
            ram_free_mb: 0,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            network_speed_mbps: None,
        }
    }

    #[test]
    fn restricted_worker_is_live_only_on_authorized_orgs() {
        let org_a = OrganizationId::now_v7();
        let org_b = OrganizationId::now_v7();
        let w = worker(Some(vec![org_a]));
        assert!(worker_live_for_org(&w, org_a));
        assert!(!worker_live_for_org(&w, org_b));
    }

    #[test]
    fn open_worker_is_live_on_any_org() {
        let w = worker(None);
        assert!(worker_live_for_org(&w, OrganizationId::now_v7()));
    }

    fn base_worker_model() -> base_worker::Model {
        base_worker::Model {
            id: BaseWorkerId::now_v7(),
            worker_id: "bw1".into(),
            display_name: "Base 1".into(),
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
            enabled: true,
            created_at: gradient_types::now(),
            ..Default::default()
        }
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
    fn active_only_patch_is_allowed_on_base_worker() {
        let body = PatchWorkerRequest {
            active: Some(true),
            ..empty_patch()
        };
        assert!(!patch_edits_base_worker_fields(&body));
        assert!(!patch_edits_base_worker_fields(&empty_patch()));
    }

    #[test]
    fn editing_name_or_caps_is_rejected_on_base_worker() {
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
            assert!(patch_edits_base_worker_fields(&body));
        }
    }

    #[test]
    fn base_worker_entry_is_flagged_and_reflects_opt_in() {
        let opted_in = base_worker_entry(base_worker_model(), true, None);
        assert!(opted_in.is_base);
        assert!(opted_in.active);

        let opted_out = base_worker_entry(base_worker_model(), false, None);
        assert!(opted_out.is_base);
        assert!(!opted_out.active);
    }
}
