/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_db::WorkerDb;
use gradient_db::caches::capacity::project_caches_all_full;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use tracing::info;

use crate::Scheduler;
use crate::jobs::eval_job_key;

/// A job already handed to the queue is left alone, so no worker claims a parked evaluation.
pub(crate) async fn park_on_full_caches(
    scheduler: &Scheduler,
    evals: Vec<MEvaluation>,
) -> anyhow::Result<Vec<MEvaluation>> {
    let db = &scheduler.db.worker_db;
    let keys = evals.iter().map(|e| eval_job_key(e.id, 0)).collect();
    let untracked: HashSet<String> = scheduler.untracked(keys).await.into_iter().collect();
    let (fresh, handed_out): (Vec<MEvaluation>, Vec<MEvaluation>) = evals
        .into_iter()
        .partition(|e| untracked.contains(&eval_job_key(e.id, 0)));

    let projects = task_projects(db, &fresh).await?;
    let full = full_projects(
        db,
        projects.values().copied().collect(),
        scheduler.db.config.cache.max_storage_gb,
    )
    .await?;

    let mut open = handed_out;
    for eval in fresh {
        let project = eval.task.and_then(|task| projects.get(&task));
        if project.is_some_and(|p| full.contains(p)) {
            info!(evaluation_id = %eval.id, "parking evaluation: every writable cache is full");
            gradient_ci::park_storage_full(db, eval).await?;
        } else {
            open.push(eval);
        }
    }
    Ok(open)
}

async fn task_projects(
    db: &WorkerDb,
    evals: &[MEvaluation],
) -> Result<HashMap<TaskId, ProjectId>, sea_orm::DbErr> {
    let tasks: HashSet<TaskId> = evals.iter().filter_map(|e| e.task).collect();
    if tasks.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<(TaskId, ProjectId)> = ETask::find()
        .select_only()
        .columns([CTask::Id, CTask::Project])
        .filter(CTask::Id.is_in(tasks))
        .into_tuple()
        .all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

async fn full_projects(
    db: &WorkerDb,
    projects: HashSet<ProjectId>,
    instance_max_storage_gb: u32,
) -> Result<HashSet<ProjectId>, sea_orm::DbErr> {
    let mut full = HashSet::new();
    for project in projects {
        if project_caches_all_full(db, project, instance_max_storage_gb).await? {
            full.insert(project);
        }
    }
    Ok(full)
}
