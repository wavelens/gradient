/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod attempts;
pub mod closure;
pub mod continuations;
pub mod create;
pub mod dispatch;
pub mod keep_vm_running;
pub mod labels;
pub mod nodes;
pub mod outputs;
pub mod phases;
pub mod pull_requests;
pub mod reads;
pub mod run_again;
pub mod settle;
pub mod tokens;
pub mod upcoming;

pub use closure::program_closure;

use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{BuildAttemptId, EvaluationId, ExecJobAttemptId, MExecJob, WaitingReason};
use sea_orm::{ConnectionTrait, DbErr};
use std::sync::LazyLock;

use crate::evaluations::EvaluationItemFailureAndAbortBooleans;
use crate::sql::status::exec_in;

pub fn exec_job_waiting_reason(job: &MExecJob) -> Option<WaitingReason> {
    if job.waits_for_manual_start && job.status == ExecJobStatus::Created {
        return Some(WaitingReason::ManualStart);
    }
    job.waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
}

pub fn log_key(attempt: ExecJobAttemptId) -> BuildAttemptId {
    BuildAttemptId::new(attempt.into_inner())
}

static HAS_OPEN_EXEC_JOBS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM exec_job WHERE evaluation = $1 AND status IN ({open}) \
         AND NOT waits_for_manual_start) AS open",
        open = exec_in(&ExecJobStatus::OPEN),
    )
});

static COUNT_WAITING_EXEC_JOBS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT count(*)::bigint AS waiting FROM exec_job WHERE evaluation = $1 \
         AND status IN ({open}) AND waiting_reason IS NOT NULL AND NOT waits_for_manual_start",
        open = exec_in(&ExecJobStatus::OPEN),
    )
});

static SELECT_EXEC_JOB_FAILED_AND_ABORTED_FLAGS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM exec_job WHERE evaluation = $1 AND status IN ({failed})) \
         AS failed, \
         EXISTS (SELECT 1 FROM exec_job WHERE evaluation = $1 AND status IN ({aborted})) \
         AS aborted",
        failed = exec_in(&[ExecJobStatus::Failed]),
        aborted = exec_in(&[ExecJobStatus::Aborted]),
    )
});

crate::sql_lazy! {
    HAS_OPEN_EXEC_JOBS = || HAS_OPEN_EXEC_JOBS_SQL.as_str(),
        params = [EvaluationId];
    COUNT_WAITING_EXEC_JOBS = || COUNT_WAITING_EXEC_JOBS_SQL.as_str(),
        params = [EvaluationId];
    SELECT_EXEC_JOB_FAILED_AND_ABORTED_FLAGS = || SELECT_EXEC_JOB_FAILED_AND_ABORTED_FLAGS_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn has_open_exec_jobs<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    HAS_OPEN_EXEC_JOBS
        .scalar(db, [evaluation.into()], "open")
        .await
}

pub async fn count_waiting_exec_jobs<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<u32, DbErr> {
    let waiting: i64 = COUNT_WAITING_EXEC_JOBS
        .scalar(db, [evaluation.into()], "waiting")
        .await?;
    Ok(u32::try_from(waiting).unwrap_or(u32::MAX))
}

pub async fn read_exec_jobs_failure_and_abort_flags<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<EvaluationItemFailureAndAbortBooleans, DbErr> {
    EvaluationItemFailureAndAbortBooleans::read(
        db,
        &SELECT_EXEC_JOB_FAILED_AND_ABORTED_FLAGS,
        evaluation,
    )
    .await
}
