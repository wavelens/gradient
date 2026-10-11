/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ActionError;
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_db::lookup::by_id;
use gradient_types::{ECommit, EProject, ETask, MCommit, MEvaluation, MTask, ProjectId};

pub(super) struct EvaluationRows {
    pub task: MTask,
    pub commit: MCommit,
}

pub(super) async fn load_evaluation_rows(
    ctx: &CiContext,
    evaluation: &MEvaluation,
) -> Result<EvaluationRows> {
    let task_id = evaluation
        .task
        .ok_or_else(|| anyhow!("evaluation has no task (direct build)"))?;

    let task = by_id::<ETask, _>(&ctx.db.worker_db, task_id)
        .await
        .context("load task")?
        .ok_or(ActionError::TaskNotFound { id: task_id })?;

    let commit = by_id::<ECommit, _>(&ctx.db.worker_db, evaluation.commit)
        .await
        .context("load commit")?
        .ok_or_else(|| anyhow!("commit {} not found", evaluation.commit))?;

    Ok(EvaluationRows { task, commit })
}

pub(super) async fn load_project_name(ctx: &CiContext, project: ProjectId) -> Option<String> {
    by_id::<EProject, _>(&ctx.db.worker_db, project)
        .await
        .ok()
        .flatten()
        .map(|p| p.name)
}
