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

use crate::error::WebResult;
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
