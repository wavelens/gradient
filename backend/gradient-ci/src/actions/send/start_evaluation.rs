/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ActionError;
use crate::actions::ExecutorOk;
use crate::context::CiContext;
use crate::{park_if_no_cache, park_if_no_workers, park_if_storage_full, trigger_evaluation};
use anyhow::{Context, Result, anyhow, bail};
use gradient_db::lookup::by_id;
use gradient_entity::evaluation::WalkMode;
use gradient_sources::HeadCommit;
use gradient_types::branch_key::NO_BRANCH;
use gradient_types::{CEvaluation, EEvaluation, ETask, ExecJobId, MEvaluation, MTask, TaskId};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::Value as JsonValue;

pub(crate) async fn execute_start_evaluation(
    ctx: &CiContext,
    payload: &JsonValue,
    action_task: TaskId,
    target: TaskId,
) -> Result<ExecutorOk> {
    let task = task_to_start(ctx, action_task, target).await?;
    let (_, head) = gradient_sources::head_commit(&ctx.db, &task, None)
        .await
        .context("resolve the head commit of the task to start")?;
    let parent = parent_job_of(payload);

    let txn = ctx
        .db
        .worker_db
        .begin()
        .await
        .context("begin the evaluation start")?;
    let evaluation = queue_evaluation(&txn, &task, head, parent).await?;
    txn.commit().await.context("commit the evaluation start")?;

    let evaluation = park_if_blocked(ctx, evaluation, &task).await?;
    ctx.db.record_evaluation_created(&evaluation).await;
    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: Some(serde_json::json!({ "evaluation": evaluation.id }).to_string()),
    })
}

async fn task_to_start(ctx: &CiContext, action_task: TaskId, target: TaskId) -> Result<MTask> {
    let load = |id: TaskId| async move {
        by_id::<ETask, _>(&ctx.db.worker_db, id)
            .await
            .context("load task")?
            .ok_or(anyhow::Error::from(ActionError::TaskNotFound { id }))
    };
    let owner = load(action_task).await?;
    let task = load(target).await?;
    if task.project != owner.project {
        bail!("the task to start belongs to another project");
    }
    Ok(task)
}

fn parent_job_of(payload: &JsonValue) -> Option<ExecJobId> {
    payload
        .get("job")
        .and_then(JsonValue::as_str)
        .and_then(|job| job.parse().ok())
}

pub(crate) async fn queue_evaluation<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    head: HeadCommit,
    parent: Option<ExecJobId>,
) -> Result<MEvaluation> {
    let evaluation = trigger_evaluation(
        db,
        task,
        head.hash,
        Some(head.message),
        Some(head.author_name),
        None,
        true,
        NO_BRANCH.to_owned(),
        None,
        None,
        None,
        None,
        WalkMode::default(),
    )
    .await
    .map_err(|e| anyhow!("queue the evaluation: {e}"))?;

    if let Some(parent) = parent {
        EEvaluation::update_many()
            .col_expr(CEvaluation::ParentExecJob, Expr::value(parent))
            .filter(CEvaluation::Id.eq(evaluation.id))
            .exec(db)
            .await
            .context("record the parent exec job")?;
    }
    Ok(evaluation)
}

async fn park_if_blocked(
    ctx: &CiContext,
    evaluation: MEvaluation,
    task: &MTask,
) -> Result<MEvaluation> {
    let db = &ctx.db.worker_db;
    let max_storage_gb = ctx.db.config.cache.max_storage_gb;
    let evaluation = park_if_no_cache(db, evaluation, task.project).await?;
    let evaluation = park_if_storage_full(db, evaluation, task.project, max_storage_gb).await?;
    Ok(park_if_no_workers(db, evaluation, task.project).await?)
}
