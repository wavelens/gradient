/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_types::{ECommit, EProject, ETask, MCommit, MEvaluation, MTask};
use sea_orm::EntityTrait;

pub(super) struct EvaluationRows {
    pub task: MTask,
    pub project_name: Option<String>,
    pub commit: MCommit,
}

pub(super) async fn load_evaluation_rows(
    ctx: &CiContext,
    evaluation: &MEvaluation,
) -> Result<EvaluationRows> {
    let task_id = evaluation
        .task
        .ok_or_else(|| anyhow!("evaluation has no task (direct build)"))?;

    let task = ETask::find_by_id(task_id)
        .one(&ctx.db.worker_db)
        .await
        .context("loading task")?
        .ok_or_else(|| anyhow!("task {} not found", task_id))?;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&ctx.db.worker_db)
        .await
        .context("loading commit")?
        .ok_or_else(|| anyhow!("commit {} not found", evaluation.commit))?;

    let project_name = EProject::find_by_id(task.project)
        .one(&ctx.db.worker_db)
        .await
        .ok()
        .flatten()
        .map(|p| p.name);

    Ok(EvaluationRows {
        task,
        project_name,
        commit,
    })
}
