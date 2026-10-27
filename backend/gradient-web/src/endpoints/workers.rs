/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_types::{BaseResponse, MUser};
use std::sync::Arc;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;

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
    require_worker_access(&state, &user)?;
    let workers = scheduler.workers_info().await;
    Ok(ok_json(workers))
}

#[utoipa::path(
    get,
    path = "/workers/{worker}",
    operation_id = "getWorker",
    tag = "admin",
    summary = "Get connected worker",
    description = "Returns one connected worker with its capabilities, assigned job counts, exec labels and exec devices. Device `free` counts leave out devices held by assigned exec jobs.\n\nSame access as `listWorkers`. Answers 404 when no worker with this id is connected.",
    params(("worker" = String, Path)),
    responses((status = OK, description = "Worker", body = WorkerInfo), WebError)
)]
pub async fn get_worker(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(worker): Path<String>,
) -> WebResult<Json<BaseResponse<WorkerInfo>>> {
    require_worker_access(&state, &user)?;
    let info = scheduler
        .workers_info()
        .await
        .into_iter()
        .find(|w| w.id == worker)
        .ok_or_else(|| WebError::not_found(Resource::Worker))?;
    Ok(ok_json(info))
}

fn require_worker_access(state: &ServerState, user: &MUser) -> WebResult<()> {
    if !state.config.server.public_stats && !user.superuser {
        return Err(WorkerRefusal::SuperuserRequired.into());
    }
    Ok(())
}
