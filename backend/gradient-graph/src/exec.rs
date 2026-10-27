/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::exec_jobs::{attempts, phases, reads, settle};
use gradient_db::status::check_evaluation_done;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::events::exec_job::Reported;
use gradient_types::ids::{DispatchedJobId, EvaluationId, ExecJobAttemptId, ExecJobId};
use gradient_types::{MExecJob, MExecJobAttempt};
use tracing::info;

use crate::policy::MAX_WORKER_LOSSES;

const TOO_MANY_LOSSES: &str = "the exec job lost its worker too many times in a row";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterAttempt {
    Requeue { consume_retry: bool },
    End(ExecJobStatus),
}

pub(crate) fn after_attempt(
    reason: AttemptEndReason,
    retries_left: i32,
    losses_in_a_row: i64,
) -> AfterAttempt {
    match reason {
        AttemptEndReason::Completed => AfterAttempt::End(ExecJobStatus::Completed),
        AttemptEndReason::Aborted => AfterAttempt::End(ExecJobStatus::Aborted),
        AttemptEndReason::Lost if losses_in_a_row >= MAX_WORKER_LOSSES => {
            AfterAttempt::End(ExecJobStatus::Failed)
        }
        AttemptEndReason::Lost => AfterAttempt::Requeue {
            consume_retry: false,
        },
        AttemptEndReason::Failed | AttemptEndReason::Timeout if retries_left > 0 => {
            AfterAttempt::Requeue {
                consume_retry: true,
            }
        }
        AttemptEndReason::Failed | AttemptEndReason::Timeout => {
            AfterAttempt::End(ExecJobStatus::Failed)
        }
    }
}

pub(crate) struct Assignment {
    pub exec_job: ExecJobId,
    pub attempt: ExecJobAttemptId,
    pub number: u32,
    pub assignment: DispatchedJobId,
    pub worker: String,
    pub retries_cap: u32,
}

pub(crate) async fn settle(ctx: &DbContext) -> Result<()> {
    let refused = settle::refused(&ctx.worker_db, ctx.config.exec.enable).await?;
    let settled = settle::waiting(&ctx.worker_db).await?;
    for job in &settled.queued {
        announce(ctx, job, None, None).await;
    }

    let failed: Vec<_> = refused.iter().chain(&settled.failed).collect();
    for job in &failed {
        announce(ctx, job, None, None).await;
    }

    finish_evaluations(ctx, failed.iter().map(|job| job.evaluation)).await
}

pub(crate) async fn assigned(ctx: &DbContext, assignment: Assignment) -> Result<bool> {
    let opened = attempts::open(
        &ctx.worker_db,
        assignment.exec_job,
        assignment.attempt,
        i32::try_from(assignment.number).unwrap_or(i32::MAX),
        assignment.assignment,
        &assignment.worker,
        i32::try_from(assignment.retries_cap).unwrap_or(i32::MAX),
    )
    .await?;
    Ok(opened)
}

pub(crate) async fn program_started(ctx: &DbContext, attempt: ExecJobAttemptId) -> Result<()> {
    if let Some(job) = attempts::program_started(&ctx.worker_db, attempt).await? {
        announce(ctx, &job, Some(attempt), None).await;
    }

    Ok(())
}

pub(crate) async fn phase(
    ctx: &DbContext,
    attempt: ExecJobAttemptId,
    name: String,
    payload: Vec<u8>,
) -> Result<()> {
    if let Some(job) = phases::record(&ctx.worker_db, attempt, name.clone(), payload).await? {
        announce(ctx, &job, Some(attempt), Some(name)).await;
    }

    Ok(())
}

pub(crate) async fn attempt_ended(
    ctx: &DbContext,
    attempt: ExecJobAttemptId,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Result<()> {
    let Some((job, closed)) =
        attempts::close(&ctx.worker_db, attempt, reason, message.clone()).await?
    else {
        info!(%attempt, ?reason, "report for a closed exec attempt; ignored");
        return Ok(());
    };

    let losses = match reason {
        AttemptEndReason::Lost => attempts::losses_in_a_row(&ctx.worker_db, job.id).await?,
        _ => 0,
    };
    match after_attempt(reason, job.retries_left, losses) {
        AfterAttempt::Requeue { consume_retry } => requeue(ctx, &job, &closed, consume_retry).await,
        AfterAttempt::End(status) => {
            let message = match (status, reason) {
                (ExecJobStatus::Failed, AttemptEndReason::Lost) => Some(TOO_MANY_LOSSES.to_owned()),
                (ExecJobStatus::Completed, _) => None,
                _ => message,
            };
            end(ctx, job.id, status, message, Some(closed.id)).await?;
            Ok(())
        }
    }
}

pub(crate) async fn abort(ctx: &DbContext, exec_job: ExecJobId) -> Result<bool> {
    let open = attempts::open_of_job(&ctx.worker_db, exec_job).await?;
    let attempt = match open {
        Some(open) => attempts::close(&ctx.worker_db, open.id, AttemptEndReason::Aborted, None)
            .await?
            .map(|(_, closed)| closed.id),
        None => None,
    };

    end(ctx, exec_job, ExecJobStatus::Aborted, None, attempt).await
}

pub(crate) async fn abort_evaluation(
    ctx: &DbContext,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobId>> {
    let mut aborted = Vec::new();
    for job in reads::open_of_evaluation(&ctx.worker_db, evaluation).await? {
        if abort(ctx, job).await? {
            aborted.push(job);
        }
    }

    Ok(aborted)
}

async fn requeue(
    ctx: &DbContext,
    job: &MExecJob,
    closed: &MExecJobAttempt,
    consume_retry: bool,
) -> Result<()> {
    if let Some(job) = attempts::requeue(&ctx.worker_db, job.id, consume_retry).await? {
        announce(ctx, &job, Some(closed.id), None).await;
    }

    Ok(())
}

async fn end(
    ctx: &DbContext,
    exec_job: ExecJobId,
    status: ExecJobStatus,
    message: Option<String>,
    attempt: Option<ExecJobAttemptId>,
) -> Result<bool> {
    let Some(job) = attempts::finish(&ctx.worker_db, exec_job, status, message).await? else {
        return Ok(false);
    };

    announce(ctx, &job, attempt, None).await;
    check_evaluation_done(ctx, job.evaluation).await?;
    Ok(true)
}

async fn finish_evaluations(
    ctx: &DbContext,
    evaluations: impl Iterator<Item = EvaluationId>,
) -> Result<()> {
    for evaluation in evaluations.collect::<BTreeSet<_>>() {
        check_evaluation_done(ctx, evaluation).await?;
    }

    Ok(())
}

async fn announce(
    ctx: &DbContext,
    job: &MExecJob,
    attempt: Option<ExecJobAttemptId>,
    phase: Option<String>,
) {
    if phase.is_none() && Reported::reports(job.status).is_none() {
        return;
    }

    ctx.record(Reported {
        job: job.id,
        attempt,
        entry_point: job.entry_point,
        evaluation: job.evaluation,
        status: i16::from(job.status),
        phase,
        ..Default::default()
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_or_timed_out_attempt_restarts_while_retries_are_left() {
        assert_eq!(
            after_attempt(AttemptEndReason::Failed, 2, 0),
            AfterAttempt::Requeue {
                consume_retry: true
            }
        );
        assert_eq!(
            after_attempt(AttemptEndReason::Timeout, 1, 0),
            AfterAttempt::Requeue {
                consume_retry: true
            }
        );
        assert_eq!(
            after_attempt(AttemptEndReason::Failed, 0, 0),
            AfterAttempt::End(ExecJobStatus::Failed)
        );
    }

    #[test]
    fn a_lost_attempt_never_consumes_a_retry_and_ten_in_a_row_fail() {
        assert_eq!(
            after_attempt(AttemptEndReason::Lost, 0, 1),
            AfterAttempt::Requeue {
                consume_retry: false
            }
        );
        assert_eq!(
            after_attempt(AttemptEndReason::Lost, 3, 10),
            AfterAttempt::End(ExecJobStatus::Failed)
        );
    }

    #[test]
    fn completion_and_abort_are_final() {
        assert_eq!(
            after_attempt(AttemptEndReason::Completed, 3, 0),
            AfterAttempt::End(ExecJobStatus::Completed)
        );
        assert_eq!(
            after_attempt(AttemptEndReason::Aborted, 3, 0),
            AfterAttempt::End(ExecJobStatus::Aborted)
        );
    }
}
