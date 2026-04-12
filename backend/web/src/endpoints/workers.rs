/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::State;
use axum::{Extension, Json};
use core::types::{BaseResponse, ServerState};
use proto::{Scheduler, WorkerInfo};
use std::sync::Arc;

use crate::error::WebResult;

pub async fn get_workers(
    _state: State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<WorkerInfo>>>> {
    let workers = scheduler.workers_info().await;
    Ok(Json(BaseResponse { error: false, message: workers }))
}
