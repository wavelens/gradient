/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::maintenance::admin_tasks;
use gradient_db::maintenance::storage_usage::{self};
use gradient_types::*;
use uuid::Uuid;

use crate::endpoints::board_subjects::{BoardProject, board_projects};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
pub use gradient_api_types::{
    AdminStorageResponse, CacheStorageSummary, DeepGarbageCollectionStats, ProjectStoredBytes,
    TaskStoredBytes,
};

#[utoipa::path(
    get,
    path = "/storage",
    operation_id = "getStorage",
    tag = "admin",
    summary = "Get storage usage",
    description = "Get storage usage for an instance. The response fields report the total disk capacity and free space of the local NAR store. These values don't exist for S3 storage. Other fields give the size and package count of each cache, the bytes in the cache for each project and each task, the oldest evaluation for each task, and the result of the last completed deep GC. Numbers come from a computation at request time. Store paths that multiple tasks build count once per task and once per project. The request needs `superuser`.",
    responses((status = OK, description = "Storage usage", body = AdminStorageResponse), WebError)
)]
pub async fn get_storage(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<AdminStorageResponse>>> {
    let db = &state.web_db;
    let disk = state.nar_storage.local_disk_total_and_free_bytes()?;
    let (caches, project_rows, task_rows) = tokio::try_join!(
        storage_usage::get_cache_storage_usage_rows(db),
        storage_usage::get_project_storage_usage_rows(db),
        storage_usage::task_stored_bytes(db),
    )?;
    let deep_gc = admin_tasks::find_latest_finished_deep_gc_admin_task(db).await?;

    let project_ids: Vec<Uuid> = project_rows
        .iter()
        .map(|row| row.project)
        .chain(task_rows.iter().map(|row| row.project))
        .map(Into::into)
        .collect();
    let names = board_projects(db, &project_ids).await?;
    let project = |id: ProjectId| -> Option<BoardProject> { names.get(&id.into()).cloned() };

    let projects = project_rows
        .into_iter()
        .filter_map(|row| {
            Some(ProjectStoredBytes {
                project: project(row.project)?,
                packages: row.packages,
                bytes: row.bytes,
            })
        })
        .collect();
    let tasks = task_rows
        .into_iter()
        .filter_map(|row| {
            Some(TaskStoredBytes {
                id: row.task,
                project: project(row.project)?,
                name: row.name,
                display_name: row.display_name,
                keep_evaluations: row.keep_evaluations,
                evaluations: row.evaluations,
                oldest_evaluation_at: row.oldest_evaluation_at,
                packages: row.packages,
                bytes: row.bytes,
            })
        })
        .collect();
    let last_deep_gc = deep_gc.map(|task| DeepGarbageCollectionStats {
        finished_at: task.finished_at,
        ..task
            .progress
            .and_then(|progress| serde_json::from_value(progress).ok())
            .unwrap_or_default()
    });

    Ok(ok_json(AdminStorageResponse {
        disk_total_bytes: disk.map(|disk| disk.total_bytes),
        disk_free_bytes: disk.map(|disk| disk.free_bytes),
        caches: caches.into_iter().map(Into::into).collect(),
        projects,
        tasks,
        last_deep_gc,
    }))
}
