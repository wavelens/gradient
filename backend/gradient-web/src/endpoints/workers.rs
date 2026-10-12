/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_types::{BaseResponse, MUser};
use std::sync::Arc;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
enum WorkerRefusal {
    #[error("workers endpoint requires superuser")]
    SuperuserRequired,
}

impl Refusal for WorkerRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::SuperuserRequired => RefusalStatus::Forbidden,
        }
    }
}

#[utoipa::path(
    get,
    path = "/workers",
    operation_id = "listWorkers",
    tag = "admin",
    summary = "List connected workers",
    description = "Returns all workers currently connected to the server via the `/proto`\nWebSocket, including their capabilities and assigned job counts.\n\nRequires the caller to have the `superuser` flag, unless the server\nis started with `GRADIENT_PUBLIC_STATS=true`.",
    responses((status = OK, description = "Worker list", body = Vec<WorkerInfo>), WebError)
)]
pub async fn get_workers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<WorkerInfo>>>> {
    if !state.config.server.public_stats && !user.superuser {
        return Err(WorkerRefusal::SuperuserRequired.into());
    }
    let workers = scheduler.workers_info().await;
    Ok(ok_json(workers))
}
