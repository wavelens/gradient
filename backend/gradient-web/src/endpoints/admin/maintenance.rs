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
use gradient_core::ServerState;
use gradient_db::admin_tasks::{self, InsertPendingError};
use gradient_entity::ids::AdminTaskId;
use gradient_types::{AdminTaskKind, BaseResponse, MUser};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

#[derive(Serialize, Debug)]
pub struct StartDeepGcResponse {
    pub task_id: AdminTaskId,
    pub status: &'static str,
}

/// Request a deep GC round. A round already running starts over from its first
/// unit at full speed, so it covers whatever changed before this request.
pub async fn start_deep_gc(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<(StatusCode, Json<BaseResponse<StartDeepGcResponse>>)> {
    require_superuser(&user)?;
    let db = &state.worker_db;
    let task_id =
        match admin_tasks::insert_pending(db, AdminTaskKind::DeepGc, Some(user.id)).await {
            Ok(task) => task.id,
            Err(InsertPendingError::AlreadyActive(id)) => {
                admin_tasks::restart(db, id, Some(user.id))
                    .await
                    .map_err(|e| WebError::internal(format!("admin_task restart failed: {e}")))?;
                id
            }
            Err(InsertPendingError::Db(e)) => {
                return Err(WebError::internal(format!("admin_task insert failed: {e}")));
            }
        };

    info!(%task_id, "deep_gc: round requested");
    Ok((
        StatusCode::ACCEPTED,
        ok_json(StartDeepGcResponse {
            task_id,
            status: "pending",
        }),
    ))
}
