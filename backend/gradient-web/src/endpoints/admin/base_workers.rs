/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::{base_worker, project_base_worker};
use gradient_pool::WorkerInfo;
use gradient_scheduler::Scheduler;
use gradient_types::ids::ProjectId;
use gradient_types::{BaseResponse, EBaseWorker, EProjectBaseWorker, MUser};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use crate::endpoints::projects::workers::{WorkerConnection, worker_connection};
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::{OptionExt, ok_json};

#[derive(Serialize)]
pub struct BaseWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub url: Option<String>,
    pub enabled: bool,
    pub auto_enable: bool,
    pub gradient_ci: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
}

pub async fn get_base_workers(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<BaseWorkerEntry>>>> {
    require_superuser(&user)?;

    let live: HashMap<String, WorkerInfo> = scheduler
        .workers_info()
        .await
        .into_iter()
        .map(|w| (w.id.clone(), w))
        .collect();
    let rows = EBaseWorker::find()
        .order_by_asc(base_worker::Column::CreatedAt)
        .all(&state.web_db)
        .await?;

    Ok(ok_json(
        rows.into_iter()
            .map(|bw| BaseWorkerEntry {
                connection: worker_connection(&live, &scheduler.connection_failures, &bw.worker_id),
                worker_id: bw.worker_id,
                display_name: bw.display_name,
                url: bw.url,
                enabled: bw.enabled,
                auto_enable: bw.auto_enable,
                gradient_ci: bw.gradient_ci,
            })
            .collect(),
    ))
}

pub async fn delete_base_worker(
    State(state): State<Arc<ServerState>>,
    Path(worker_id): Path<String>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_superuser(&user)?;

    let bw = EBaseWorker::find()
        .filter(base_worker::Column::WorkerId.eq(&worker_id))
        .one(&state.web_db)
        .await?
        .or_not_found("base worker")?;
    if !bw.gradient_ci {
        return Err(WebError::conflict(
            "base workers are managed by server state",
        ));
    }

    let projects: HashSet<ProjectId> =
        gradient_db::projects::base_workers::projects_enabling_base_worker(&state.web_db, bw.id)
            .await?
            .into_iter()
            .collect();
    scheduler
        .abort_project_jobs_on_worker(&worker_id, &projects)
        .await;
    EProjectBaseWorker::delete_many()
        .filter(project_base_worker::Column::BaseWorker.eq(bw.id))
        .exec(&state.web_db)
        .await?;
    EBaseWorker::delete_by_id(bw.id).exec(&state.web_db).await?;
    scheduler.request_reauth(&worker_id).await;

    Ok(ok_json(format!("base worker '{worker_id}' disconnected")))
}
