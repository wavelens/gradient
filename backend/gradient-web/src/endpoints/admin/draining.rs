/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use std::sync::Arc;

use axum::{Extension, Json, extract::State};
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::BaseResponse;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::WebResult;
use crate::helpers::ok_json;

#[derive(Deserialize)]
pub struct SetDrainingRequest {
    pub enabled: bool,
}

#[derive(Serialize, Debug)]
pub struct DrainingResponse {
    pub draining: bool,
}

pub async fn set_draining(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Json(req): Json<SetDrainingRequest>,
) -> WebResult<Json<BaseResponse<DrainingResponse>>> {
    scheduler.draining.set(req.enabled).await;

    let evaluations = if req.enabled {
        gradient_db::evaluations::draining::park_active_evals(&state.worker_db).await
    } else {
        gradient_db::evaluations::draining::unpark_draining_evals(&state.worker_db).await
    }
    .context("apply the draining transition")?;

    info!(enabled = req.enabled, evaluations, "draining toggled");

    Ok(ok_json(DrainingResponse {
        draining: req.enabled,
    }))
}
