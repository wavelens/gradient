/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::State;
use axum::{Extension, Json};
use core::types::{BaseResponse, MUser, ServerState};
use proto::{Scheduler, WorkerInfo};
use std::sync::Arc;

use crate::error::{WebError, WebResult};

pub async fn get_workers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<WorkerInfo>>>> {
    if !state.cli.global_stats_public && !user.superuser {
        return Err(WebError::Forbidden("workers endpoint requires superuser".into()));
    }
    let workers = scheduler.workers_info().await;
    Ok(Json(BaseResponse { error: false, message: workers }))
}
