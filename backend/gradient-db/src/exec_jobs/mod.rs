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
use gradient_types::{BuildAttemptId, EvaluationId, ExecJobAttemptId};
use sea_orm::{ConnectionTrait, DbErr};
use std::sync::LazyLock;

use crate::sql::status::exec_in;

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

static ANY_EXEC_JOB_FAILED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM exec_job WHERE evaluation = $1 AND status IN ({failed})) \
         AS failed",
        failed = exec_in(&[ExecJobStatus::Failed, ExecJobStatus::Aborted]),
    )
});

crate::sql_lazy! {
    HAS_OPEN_EXEC_JOBS = || HAS_OPEN_EXEC_JOBS_SQL.as_str(),
        params = [EvaluationId];
    ANY_EXEC_JOB_FAILED = || ANY_EXEC_JOB_FAILED_SQL.as_str(),
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

pub async fn any_exec_job_failed<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    ANY_EXEC_JOB_FAILED
        .scalar(db, [evaluation.into()], "failed")
        .await
}
