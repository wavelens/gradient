/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /admin/tasks`, `GET /admin/tasks/{task_id}`

use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use gradient_entity::ids::{AdminTaskId, UserId};
use gradient_core::db::admin_tasks;
use gradient_core::types::{BaseResponse, MAdminTask, MUser, ServerState};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize, Debug)]
pub struct AdminTaskDto {
    pub id: AdminTaskId,
    pub kind: &'static str,
    pub status: &'static str,
    pub created_at: chrono::NaiveDateTime,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub progress: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_by: Option<UserId>,
}

impl From<MAdminTask> for AdminTaskDto {
    fn from(m: MAdminTask) -> Self {
        Self {
            id: m.id,
            kind: m.kind.as_str(),
            status: m.status.as_str(),
            created_at: m.created_at,
            started_at: m.started_at,
            finished_at: m.finished_at,
            progress: m.progress,
            error: m.error,
            created_by: m.created_by,
        }
    }
}

pub async fn list_tasks(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<AdminTaskDto>>>> {
    require_superuser(&user)?;
    let rows = admin_tasks::list_recent(&state.worker_db, 50)
        .await
        .map_err(|e| WebError::internal(format!("list_recent: {e}")))?;
    Ok(ok_json(rows.into_iter().map(AdminTaskDto::from).collect()))
}

pub async fn get_task(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(task_id): Path<AdminTaskId>,
) -> WebResult<Json<BaseResponse<AdminTaskDto>>> {
    require_superuser(&user)?;
    let row = admin_tasks::get(&state.worker_db, task_id)
        .await
        .map_err(|e| WebError::internal(format!("get_task: {e}")))?
        .ok_or_else(|| WebError::not_found("admin_task"))?;
    Ok(ok_json(AdminTaskDto::from(row)))
}
