/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use chrono::NaiveDateTime;
use gradient_db::DbContext;
use gradient_db::exec_jobs::{attempts, continuations, nodes, phases, reads, settle, tokens};
use gradient_db::status::check_evaluation_done;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::events::exec_job::Reported;
use gradient_types::ids::{DispatchedJobId, EvaluationId, ExecJobAttemptId, ExecJobId};
use gradient_types::{MExecJob, MExecJobAttempt};
use tracing::info;

use crate::messages::{ClusterNext, ExecNodeEnd};
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

pub(crate) fn node_status(reason: AttemptEndReason) -> ExecJobStatus {
    match reason {
        AttemptEndReason::Completed => ExecJobStatus::Completed,
        AttemptEndReason::Aborted => ExecJobStatus::Aborted,
        AttemptEndReason::Failed | AttemptEndReason::Timeout | AttemptEndReason::Lost => {
            ExecJobStatus::Failed
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
    pub token_hash: String,
    pub token_expires_at: NaiveDateTime,
    pub master_port: Option<u16>,
}

struct ClosedNode {
    job: MExecJob,
    attempt: ExecJobAttemptId,
    reason: AttemptEndReason,
    message: Option<String>,
}

pub(crate) async fn settle(ctx: &DbContext) -> Result<()> {
    let refused = settle::refused(&ctx.worker_db, ctx.config.exec.enable).await?;
    let settled = settle::waiting(&ctx.worker_db).await?;
    for job in &settled.queued {
        announce(ctx, job, None, None).await?;
    }

    let failed: Vec<_> = refused.iter().chain(&settled.failed).collect();
    for job in &failed {
        announce(ctx, job, None, None).await?;
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
        assignment.master_port,
    )
    .await?;
    if !opened {
        return Ok(false);
    }

    continuations::forget_recorded(&ctx.worker_db, assignment.exec_job).await?;
    tokens::insert(
        &ctx.worker_db,
        assignment.attempt,
        &assignment.token_hash,
        assignment.token_expires_at,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn program_started(ctx: &DbContext, attempt: ExecJobAttemptId) -> Result<()> {
    if let Some(job) = attempts::program_started(&ctx.worker_db, attempt).await? {
        announce(ctx, &job, Some(attempt), None).await?;
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
        announce(ctx, &job, Some(attempt), Some(name)).await?;
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
            if status == ExecJobStatus::Completed {
                queue_continuation(ctx, job.id).await?;
            }
            end(ctx, job.id, status, message, Some(closed.id)).await?;
            Ok(())
        }
    }
}

pub(crate) async fn cluster_settled(
    ctx: &DbContext,
    ends: Vec<ExecNodeEnd>,
    next: ClusterNext,
) -> Result<()> {
    let closed = close_node_attempts(ctx, ends).await?;
    let Some(first) = closed.first() else {
        info!(
            ?next,
            "settle for an exec cluster without an open attempt; ignored"
        );
        return Ok(());
    };

    let job = first.job.id;
    match next {
        ClusterNext::Restart { consume_retry } => {
            restart_cluster(ctx, job, &closed, consume_retry).await
        }
        ClusterNext::End => end_cluster(ctx, job, &closed).await,
    }
}

async fn close_node_attempts(ctx: &DbContext, ends: Vec<ExecNodeEnd>) -> Result<Vec<ClosedNode>> {
    let mut closed = Vec::new();
    for end in ends {
        if let Some((job, _)) =
            attempts::close(&ctx.worker_db, end.attempt, end.reason, end.message.clone()).await?
        {
            closed.push(ClosedNode {
                job,
                attempt: end.attempt,
                reason: end.reason,
                message: end.message,
            });
        }
    }

    Ok(closed)
}

async fn restart_cluster(
    ctx: &DbContext,
    job: ExecJobId,
    closed: &[ClosedNode],
    consume_retry: bool,
) -> Result<()> {
    let requeued = attempts::requeue_nodes(&ctx.worker_db, job, consume_retry).await?;
    if let Some(master) = requeued.iter().find(|row| row.node_index == 0) {
        let attempt = closed
            .iter()
            .find(|node| node.job.id == master.id)
            .map(|node| node.attempt);
        announce(ctx, master, attempt, None).await?;
    }

    Ok(())
}

async fn end_cluster(ctx: &DbContext, job: ExecJobId, closed: &[ClosedNode]) -> Result<()> {
    let rows = nodes::nodes_of(&ctx.worker_db, job).await?;
    let by_job: HashMap<ExecJobId, &ClosedNode> =
        closed.iter().map(|node| (node.job.id, node)).collect();
    let every_node_completed = rows.iter().all(|row| {
        by_job
            .get(&row.id)
            .is_some_and(|node| node.reason == AttemptEndReason::Completed)
    });
    if every_node_completed && let Some(master) = rows.first() {
        queue_continuation(ctx, master.id).await?;
    }

    for row in rows.iter().rev() {
        if let Some(node) = by_job.get(&row.id) {
            let status = node_status(node.reason);
            let message = (status != ExecJobStatus::Completed)
                .then(|| node.message.clone())
                .flatten();
            end(ctx, row.id, status, message, Some(node.attempt)).await?;
        }
    }

    Ok(())
}

async fn queue_continuation(ctx: &DbContext, exec_job: ExecJobId) -> Result<()> {
    if continuations::activate(&ctx.worker_db, exec_job).await? {
        info!(%exec_job, "continue file queued as an eval job");
        ctx.wake.eval_assign.notify_one();
    }

    Ok(())
}

pub(crate) async fn abort(ctx: &DbContext, exec_job: ExecJobId) -> Result<Vec<ExecJobId>> {
    let mut aborted = Vec::new();
    for row in nodes::nodes_of(&ctx.worker_db, exec_job)
        .await?
        .iter()
        .rev()
    {
        if abort_node(ctx, row.id).await? {
            aborted.push(row.id);
        }
    }

    aborted.reverse();
    Ok(aborted)
}

async fn abort_node(ctx: &DbContext, exec_job: ExecJobId) -> Result<bool> {
    let open = attempts::open_of_job(&ctx.worker_db, exec_job).await?;
    let attempt = match open {
        Some(open) => attempts::close(&ctx.worker_db, open.id, AttemptEndReason::Aborted, None)
            .await?
            .map(|(_, closed)| closed.id),
        None => None,
    };

    end(ctx, exec_job, ExecJobStatus::Aborted, None, attempt).await
}

pub(crate) async fn fail_before_start(
    ctx: &DbContext,
    exec_job: ExecJobId,
    message: &str,
) -> Result<()> {
    let Some(job) = settle::fail_open(&ctx.worker_db, exec_job, message).await? else {
        info!(%exec_job, "failure for an exec job past dispatch; ignored");
        return Ok(());
    };
    let attempt = match attempts::open_of_job(&ctx.worker_db, exec_job).await? {
        Some(open) => attempts::close(
            &ctx.worker_db,
            open.id,
            AttemptEndReason::Failed,
            Some(message.to_owned()),
        )
        .await?
        .map(|(_, closed)| closed.id),
        None => None,
    };

    announce(ctx, &job, attempt, None).await?;
    check_evaluation_done(ctx, job.evaluation).await?;
    Ok(())
}

pub(crate) async fn abort_evaluation(
    ctx: &DbContext,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobId>> {
    let mut aborted = Vec::new();
    for job in reads::open_of_evaluation(&ctx.worker_db, evaluation).await? {
        aborted.extend(abort(ctx, job).await?);
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
        announce(ctx, &job, Some(closed.id), None).await?;
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

    announce(ctx, &job, attempt, None).await?;
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
) -> Result<()> {
    let node = u32::try_from(job.node_index).unwrap_or_default();
    let reported = Reported {
        job: job.id,
        attempt,
        entry_point: job.entry_point,
        evaluation: job.evaluation,
        status: i16::from(job.status),
        node,
        ..Default::default()
    };
    if phase.is_some() {
        ctx.record(Reported { phase, ..reported }).await;
    } else if job.status.is_terminal() {
        if let Some(ended) = job_ended(ctx, job, attempt).await? {
            ctx.record(ended).await;
        }
    } else if node == 0 && Reported::reports(job.status).is_some() {
        ctx.record(reported).await;
    }

    Ok(())
}

async fn job_ended(
    ctx: &DbContext,
    job: &MExecJob,
    attempt: Option<ExecJobAttemptId>,
) -> Result<Option<Reported>> {
    let rows = nodes::nodes_of(&ctx.worker_db, job.id).await?;
    let statuses: Vec<ExecJobStatus> = rows.iter().map(|row| row.status).collect();
    let status = nodes::aggregate_status(&statuses);
    let Some(master) = rows.first().filter(|_| status.is_terminal()) else {
        return Ok(None);
    };

    Ok(Some(Reported {
        job: master.id,
        attempt: attempt.filter(|_| master.id == job.id),
        entry_point: master.entry_point,
        evaluation: master.evaluation,
        status: i16::from(status),
        ..Default::default()
    }))
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
    fn a_node_ends_by_its_attempt() {
        assert_eq!(
            node_status(AttemptEndReason::Completed),
            ExecJobStatus::Completed
        );
        assert_eq!(
            node_status(AttemptEndReason::Timeout),
            ExecJobStatus::Failed
        );
        assert_eq!(node_status(AttemptEndReason::Lost), ExecJobStatus::Failed);
        assert_eq!(
            node_status(AttemptEndReason::Aborted),
            ExecJobStatus::Aborted
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
