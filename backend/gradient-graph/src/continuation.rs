/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::exec_jobs::{continuations, create};
use gradient_db::graph::repair::{RepairScope, repair_build_graph};
use gradient_db::status::{check_evaluation_done, insert_worker_message};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::exec_job_continuation::ContinuationStatus;
use gradient_types::ids::{EvaluationId, ExecJobId};
use tracing::info;

use crate::messages::ContinuationOutcome;

const CONTINUE_MESSAGE_PREFIX: &str = "continue.json: ";

pub(crate) async fn continuation_ended(
    ctx: &DbContext,
    exec_job: ExecJobId,
    outcome: ContinuationOutcome,
) -> Result<()> {
    match outcome {
        ContinuationOutcome::Retry => Ok(()),
        ContinuationOutcome::Done => done(ctx, exec_job).await,
        ContinuationOutcome::Failed { error } => failed(ctx, exec_job, &error).await,
    }
}

async fn done(ctx: &DbContext, exec_job: ExecJobId) -> Result<()> {
    let Some(evaluation) = end(ctx, exec_job, ContinuationStatus::Done, None).await? else {
        return Ok(());
    };

    repair_build_graph(ctx, RepairScope::Eval(evaluation)).await?;
    create::for_evaluation(&ctx.worker_db, evaluation).await?;
    check_evaluation_done(ctx, evaluation).await?;
    Ok(())
}

async fn failed(ctx: &DbContext, exec_job: ExecJobId, error: &str) -> Result<()> {
    let Some(evaluation) = end(
        ctx,
        exec_job,
        ContinuationStatus::Failed,
        Some(error.to_owned()),
    )
    .await?
    else {
        return Ok(());
    };

    insert_worker_message(
        &ctx.worker_db,
        evaluation,
        MessageLevel::Error,
        format!("{CONTINUE_MESSAGE_PREFIX}{error}"),
        "nix-eval".to_owned(),
    )
    .await?;
    check_evaluation_done(ctx, evaluation).await?;
    Ok(())
}

async fn end(
    ctx: &DbContext,
    exec_job: ExecJobId,
    status: ContinuationStatus,
    message: Option<String>,
) -> Result<Option<EvaluationId>> {
    let ended = continuations::end(&ctx.worker_db, exec_job, status, message).await?;
    match ended {
        Some(row) => Ok(Some(row.evaluation)),
        None => {
            info!(%exec_job, ?status, "end of a continuation that is not queued; ignored");
            Ok(None)
        }
    }
}
