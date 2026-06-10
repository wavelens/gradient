/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::State;
use axum::{Extension, Json};
use gradient_types::{BaseResponse, MUser};
use gradient_core::ServerState;
use gradient_scheduler::{Scheduler, WorkerInfo};
use std::sync::Arc;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;

pub async fn get_workers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<WorkerInfo>>>> {
    if !state.config.proto.global_stats_public && !user.superuser {
        return Err(WebError::forbidden("workers endpoint requires superuser"));
    }
    let workers = scheduler.workers_info().await;
    Ok(ok_json(workers))
}
