/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use anyhow::Context;
use axum::http::StatusCode;
use axum::{Extension, Json, extract::State};
use gradient_core::ServerState;
use gradient_db::maintenance::admin_tasks::{self, InsertPendingError};
use gradient_entity::ids::AdminTaskId;
use gradient_types::{AdminTaskKind, BaseResponse, MUser};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct StartDeepGcResponse {
    pub task_id: AdminTaskId,
    pub status: &'static str,
}

#[utoipa::path(
    post,
    path = "/maintenance/deep-gc",
    operation_id = "startDeepGc",
    tag = "admin",
    summary = "Request a deep GC round",
    description = "Requests a reconciliation between the database and every storage\nbackend (NAR files, build-request blobs, build logs, upload partials),\nrun without pausing between units. Returns `202` immediately with the\ntask id; poll `GET /admin/tasks/{task_id}` for progress and final state.\n\nWhen a `deep_gc` round is already `pending` or `running`, that round\nstarts over from its first unit and its id is returned.\n\nRequires `superuser`.",
    responses((status = ACCEPTED, description = "Round requested", body = StartDeepGcResponse), WebError)
)]
pub async fn start_deep_gc(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<(StatusCode, Json<BaseResponse<StartDeepGcResponse>>)> {
    let db = &state.worker_db;
    let task_id = match admin_tasks::insert_pending(db, AdminTaskKind::DeepGc, Some(user.id)).await
    {
        Ok(task) => task.id,
        Err(InsertPendingError::AlreadyActive(id)) => {
            admin_tasks::restart(db, id, Some(user.id))
                .await
                .context("restart admin_task")?;
            id
        }
        Err(InsertPendingError::Db(e)) => {
            return Err(anyhow::anyhow!("admin_task insert failed: {e}").into());
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
