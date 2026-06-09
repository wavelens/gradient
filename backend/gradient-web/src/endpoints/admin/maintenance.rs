/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /admin/maintenance/deep-gc`

use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::http::StatusCode;
use axum::{Extension, Json, extract::State};
use gradient_cache::cacher::run_deep_gc;
use gradient_entity::ids::AdminTaskId;
use gradient_core::db::admin_tasks::{self, InsertPendingError};
use gradient_core::types::{AdminTaskKind, BaseResponse, MUser, ServerState};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

#[derive(Serialize, Debug)]
pub struct StartDeepGcResponse {
    pub task_id: AdminTaskId,
    pub status: &'static str,
}

pub async fn start_deep_gc(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<(StatusCode, Json<BaseResponse<StartDeepGcResponse>>)> {
    require_superuser(&user)?;
    match admin_tasks::insert_pending(&state.worker_db, AdminTaskKind::DeepGc, Some(user.id)).await
    {
        Ok(task) => {
            info!(task_id = %task.id, "deep_gc: spawning sweep");
            state
                .shutdown
                .spawn(run_deep_gc(Arc::clone(&state), task.id));
            let body = ok_json(StartDeepGcResponse {
                task_id: task.id,
                status: "pending",
            });
            Ok((StatusCode::ACCEPTED, body))
        }
        Err(InsertPendingError::AlreadyActive(id)) => Err(WebError::conflict(format!(
            "deep_gc task {id} is already pending or running"
        ))),
        Err(InsertPendingError::Db(e)) => {
            Err(WebError::internal(format!("admin_task insert failed: {e}")))
        }
    }
}
