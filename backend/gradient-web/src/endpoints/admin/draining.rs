/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /admin/draining` - toggle the instance draining state.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::{Extension, Json, extract::State};
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::{BaseResponse, MUser};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;

#[derive(Deserialize)]
pub struct SetDrainingRequest {
    pub enabled: bool,
}

#[derive(Serialize, Debug)]
pub struct DrainingResponse {
    pub draining: bool,
}

/// Enable or disable draining. Enabling pauses dispatch and parks every
/// in-flight evaluation; disabling recovers the parked evaluations to `Queued`.
/// In-memory only, so draining always clears on the next server startup.
pub async fn set_draining(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Extension(user): Extension<MUser>,
    Json(req): Json<SetDrainingRequest>,
) -> WebResult<Json<BaseResponse<DrainingResponse>>> {
    require_superuser(&user)?;

    scheduler.draining.store(req.enabled, Ordering::Relaxed);

    let evaluations = if req.enabled {
        gradient_db::park_active_evals(&state.worker_db).await
    } else {
        gradient_db::unpark_draining_evals(&state.worker_db).await
    }
    .map_err(|e| WebError::internal(format!("draining transition failed: {e}")))?;

    info!(enabled = req.enabled, evaluations, "draining toggled");

    Ok(ok_json(DrainingResponse {
        draining: req.enabled,
    }))
}
