/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod attempts;
pub mod create;
pub mod dispatch;
pub mod phases;
pub mod reads;
pub mod settle;
pub mod upcoming;

use gradient_entity::runner_job::RunnerJobStatus;
use gradient_types::{BuildAttemptId, EvaluationId, RunnerJobAttemptId};
use sea_orm::{ConnectionTrait, DbErr, Value};
use std::sync::LazyLock;

use crate::sql::status::runner_in;

pub fn log_key(attempt: RunnerJobAttemptId) -> BuildAttemptId {
    BuildAttemptId::new(attempt.into_inner())
}

static HAS_OPEN_RUNNER_JOBS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM runner_job WHERE evaluation = $1 AND status IN ({open})) \
         AS open",
        open = runner_in(&RunnerJobStatus::OPEN),
    )
});

static ANY_RUNNER_JOB_FAILED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM runner_job WHERE evaluation = $1 AND status IN ({failed})) \
         AS failed",
        failed = runner_in(&[RunnerJobStatus::Failed, RunnerJobStatus::Aborted]),
    )
});

crate::sql_lazy! {
    HAS_OPEN_RUNNER_JOBS = || HAS_OPEN_RUNNER_JOBS_SQL.as_str(),
        params = [EvaluationId];
    ANY_RUNNER_JOB_FAILED = || ANY_RUNNER_JOB_FAILED_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn has_open_runner_jobs<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &HAS_OPEN_RUNNER_JOBS, evaluation, "open").await
}

pub async fn any_runner_job_failed<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &ANY_RUNNER_JOB_FAILED, evaluation, "failed").await
}

async fn flag<C: ConnectionTrait>(
    db: &C,
    query: &crate::sql::Query,
    evaluation: EvaluationId,
    column: &str,
) -> Result<bool, DbErr> {
    db.query_one_raw(query.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("{} returned no row", query.name)))?
        .try_get::<bool>("", column)
}
