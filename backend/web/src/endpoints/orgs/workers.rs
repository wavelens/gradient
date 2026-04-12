/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{NaiveDateTime, Utc};
use core::db::get_organization_by_name;
use core::types::{BaseResponse, MUser, ServerState};
use entity::worker_registration::{self, ActiveModel as AWorkerRegistration, Entity as EWorkerRegistration};
use proto::{Scheduler, WorkerInfo};
use rand::RngCore;
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
}

#[derive(Serialize)]
pub struct RegisterWorkerResponse {
    pub peer_id: Uuid,
    pub token: String,
}

#[derive(Serialize)]
pub struct OrgWorkerEntry {
    pub worker_id: String,
    pub registered_at: NaiveDateTime,
    /// Present when the worker is currently connected to this server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
}

#[derive(Serialize)]
pub struct WorkerLiveInfo {
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
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisterWorkerResponse>>> {
    let org = get_organization_by_name(Arc::clone(&state), user.id, organization)
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?
        .ok_or_else(|| WebError::not_found("organization"))?;

    if body.worker_id.is_empty() {
        return Err(WebError::BadRequest("worker_id must not be empty".into()));
    }

    // Generate a cryptographically random 32-byte token, hex-encoded.
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let token = hex::encode(raw);

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let row = AWorkerRegistration {
        id: Set(Uuid::new_v4()),
        peer_id: Set(org.id),
        worker_id: Set(body.worker_id),
        token_hash: Set(token_hash),
        created_at: Set(Utc::now().naive_utc()),
    };
    row.insert(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: RegisterWorkerResponse {
            peer_id: org.id,
            token,
        },
    }))
}

pub async fn get_org_workers(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<OrgWorkerEntry>>>> {
    let org = get_organization_by_name(Arc::clone(&state), user.id, organization)
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?
        .ok_or_else(|| WebError::not_found("organization"))?;

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
                registered_at: reg.created_at,
                live,
            }
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: entries,
    }))
}

pub async fn delete_org_worker(
    state: State<Arc<ServerState>>,
    Path((organization, worker_id)): Path<(String, String)>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<String>>> {
    let org = get_organization_by_name(Arc::clone(&state), user.id, organization)
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?
        .ok_or_else(|| WebError::not_found("organization"))?;

    let result = EWorkerRegistration::delete_many()
        .filter(worker_registration::Column::PeerId.eq(org.id))
        .filter(worker_registration::Column::WorkerId.eq(&worker_id))
        .exec(&state.db)
        .await?;

    if result.rows_affected == 0 {
        return Err(WebError::not_found("worker registration"));
    }

    Ok(Json(BaseResponse {
        error: false,
        message: format!("worker '{}' unregistered", worker_id),
    }))
}
