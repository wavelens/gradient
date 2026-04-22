/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::load_org_member;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use base64::Engine as _;
use chrono::{NaiveDateTime, Utc};
use core::types::proto::GradientCapabilities;
use core::types::{BaseResponse, MUser, ServerState};
use entity::worker_registration::{
    self, ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration,
};
use rand::RngExt as _;
use scheduler::{Scheduler, WorkerInfo};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use crate::error::{WebError, WebResult};

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
}

#[derive(Serialize)]
pub struct RegisterWorkerResponse {
    pub peer_id: Uuid,
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
    pub created_by: Option<Uuid>,
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
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisterWorkerResponse>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let worker_uuid = Uuid::parse_str(&body.worker_id)
        .ok()
        .filter(|u| u.get_version() == Some(uuid::Version::Random))
        .ok_or_else(|| WebError::BadRequest("worker_id must be a valid UUID v4".into()))?;
    let worker_id_str = worker_uuid.to_string();

    // Resolve token: use caller-supplied one (after validation) or generate a new one.
    let (token, return_token) = if let Some(provided) = body.token {
        let t = provided.trim().to_string();
        // Must be exactly 64 chars of valid standard base64 (openssl rand -base64 48 output).
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&t)
            .map_err(|_| WebError::BadRequest("token is not valid base64".into()))?;
        if decoded.len() != 48 {
            return Err(WebError::BadRequest(
                "token must be 48 raw bytes encoded as base64 (openssl rand -base64 48)".into(),
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

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let row = AWorkerRegistration {
        id: Set(Uuid::new_v4()),
        peer_id: Set(org.id),
        worker_id: Set(worker_id_str.clone()),
        token_hash: Set(token_hash),
        managed: Set(false),
        url: Set(body.url),
        display_name: Set(body.display_name.trim().to_string()),
        active: Set(true),
        created_by: Set(Some(user.id)),
        created_at: Set(Utc::now().naive_utc()),
    };
    row.insert(&state.db).await?;

    // Trigger re-auth if the worker is already connected, so it picks up
    // the new peer registration without requiring a reconnect.
    scheduler.request_reauth(&worker_id_str).await;

    Ok(Json(BaseResponse {
        error: false,
        message: RegisterWorkerResponse {
            peer_id: org.id,
            token: if return_token { Some(token) } else { None },
        },
    }))
}

pub async fn get_org_workers(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<OrgWorkerEntry>>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let registrations = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .all(&state.db)
        .await?;

    // Build a map of worker_id → live info from the scheduler.
    let live_workers: std::collections::HashMap<String, WorkerInfo> = scheduler
        .workers_info()
        .await
        .into_iter()
        .map(|w| (w.id.clone(), w))
        .collect();

    let entries = registrations
        .into_iter()
        .map(|reg| {
            let live = live_workers.get(&reg.worker_id).map(|w| WorkerLiveInfo {
                capabilities: w.capabilities.clone(),
                // architectures/system_features are only non-empty for build-capable workers
                // (WorkerCapabilities is only sent when `build` is negotiated)
                architectures: w.architectures.clone(),
                system_features: w.system_features.clone(),
                max_concurrent_builds: w.max_concurrent_builds,
                assigned_job_count: w.assigned_job_count,
                draining: w.draining,
            });
            OrgWorkerEntry {
                worker_id: reg.worker_id,
                display_name: reg.display_name,
                registered_at: reg.created_at,
                active: reg.active,
                url: reg.url,
                created_by: reg.created_by,
                live,
            }
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: entries,
    }))
}

pub async fn patch_org_worker(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let reg = EWorkerRegistration::find()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("worker registration"))?;

    let mut active_model: AWorkerRegistration = reg.into();

    if let Some(active) = body.active {
        active_model.active = Set(active);
    }
    if let Some(ref name) = body.display_name {
        active_model.display_name = Set(name.trim().to_string());
    }
    active_model.update(&state.db).await?;

    // When deactivating: abort in-flight jobs from this org on the worker
    // before triggering reauth, so the worker stops them immediately.
    if let Some(false) = body.active {
        let org_set = std::collections::HashSet::from([org.id]);
        scheduler
            .abort_org_jobs_on_worker(&worker_id, &org_set)
            .await;
    }

    // Trigger re-auth so the worker's authorized peer set is updated
    // (or the worker is kicked if all registrations are now inactive).
    if body.active.is_some() {
        scheduler.request_reauth(&worker_id).await;
    }

    Ok(Json(BaseResponse {
        error: false,
        message: format!("worker '{}' updated", worker_id),
    }))
}

pub async fn delete_org_worker(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = load_org_member(&state, user.id, organization).await?;

    let result = EWorkerRegistration::delete_many()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .exec(&state.db)
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

    Ok(Json(BaseResponse {
        error: false,
        message: format!("worker '{}' unregistered", worker_id),
    }))
}
