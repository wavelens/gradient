/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::runner_job::RunnerJobStatus;
use gradient_entity::runner_job_attempt::AttemptEndReason;
use gradient_types::{
    CRunnerJobAttempt, DispatchedJobId, ERunnerJob, ERunnerJobAttempt, MRunnerJob,
    MRunnerJobAttempt, RunnerJobAttemptId, RunnerJobId,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, Value,
};
use std::sync::LazyLock;

use crate::sql::status::runner;

static OPEN_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE runner_job SET status = {starting}, waiting_reason = NULL, \
         retries_left = LEAST(retries_left, $2) \
         WHERE id = $1 AND status = {queued}",
        starting = runner(RunnerJobStatus::Starting),
        queued = runner(RunnerJobStatus::Queued),
    )
});

static PROGRAM_STARTED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE runner_job r SET status = {running}, \
         started_at = COALESCE(r.started_at, (now() AT TIME ZONE 'UTC')) \
         FROM runner_job_attempt a \
         WHERE a.id = $1 AND a.ended_at IS NULL AND r.id = a.runner_job AND r.status = {starting} \
         RETURNING r.*",
        running = runner(RunnerJobStatus::Running),
        starting = runner(RunnerJobStatus::Starting),
    )
});

static LOSSES_IN_A_ROW_SQL: LazyLock<String> = LazyLock::new(|| {
    let lost = i16::from(AttemptEndReason::Lost);
    format!(
        "SELECT count(*) AS losses FROM runner_job_attempt a \
         WHERE a.runner_job = $1 AND a.end_reason = {lost} \
         AND a.number > COALESCE((SELECT max(b.number) FROM runner_job_attempt b \
              WHERE b.runner_job = $1 AND b.end_reason <> {lost}), 0)"
    )
});

crate::sql_lazy! {
    OPEN_RUNNER_JOB_ATTEMPT = || OPEN_SQL.as_str(),
        params = [NewUuid, Int(3)];
    RUNNER_PROGRAM_STARTED = || PROGRAM_STARTED_SQL.as_str(),
        params = [NewUuid];
    RUNNER_LOSSES_IN_A_ROW = || LOSSES_IN_A_ROW_SQL.as_str(),
        params = [NewUuid];
}

crate::sql! {
    CLOSE_RUNNER_JOB_ATTEMPT = "UPDATE runner_job_attempt \
         SET ended_at = (now() AT TIME ZONE 'UTC'), end_reason = $2, message = $3 \
         WHERE id = $1 AND ended_at IS NULL RETURNING *",
        params = [NewUuid, Int(3), Text("lost")];
}

pub async fn open<C: ConnectionTrait>(
    db: &C,
    job: RunnerJobId,
    attempt: RunnerJobAttemptId,
    number: i32,
    assignment: DispatchedJobId,
    worker: &str,
    retries_cap: i32,
) -> Result<bool, DbErr> {
    let started = db
        .execute_raw(OPEN_RUNNER_JOB_ATTEMPT.bind([
            Value::Uuid(Some(job.into_inner())),
            Value::Int(Some(retries_cap)),
        ]))
        .await?;
    if started.rows_affected() != 1 {
        return Ok(false);
    }

    crate::rows::insert::<ERunnerJobAttempt, _>(
        db,
        MRunnerJobAttempt {
            id: attempt,
            runner_job: job,
            assignment_id: assignment,
            worker: worker.to_owned(),
            number,
            started_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await?;
    Ok(true)
}

pub async fn program_started<C: ConnectionTrait>(
    db: &C,
    attempt: RunnerJobAttemptId,
) -> Result<Option<MRunnerJob>, DbErr> {
    MRunnerJob::find_by_statement(
        RUNNER_PROGRAM_STARTED.bind([Value::Uuid(Some(attempt.into_inner()))]),
    )
    .one(db)
    .await
}

pub async fn close<C: ConnectionTrait>(
    db: &C,
    attempt: RunnerJobAttemptId,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Result<Option<(MRunnerJob, MRunnerJobAttempt)>, DbErr> {
    let Some(closed) = MRunnerJobAttempt::find_by_statement(CLOSE_RUNNER_JOB_ATTEMPT.bind([
        Value::Uuid(Some(attempt.into_inner())),
        Value::SmallInt(Some(i16::from(reason))),
        Value::String(message),
    ]))
    .one(db)
    .await?
    else {
        return Ok(None);
    };
    let job = ERunnerJob::find_by_id(closed.runner_job)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("runner job {}", closed.runner_job)))?;
    Ok(Some((job, closed)))
}

pub async fn losses_in_a_row<C: ConnectionTrait>(db: &C, job: RunnerJobId) -> Result<i64, DbErr> {
    db.query_one_raw(RUNNER_LOSSES_IN_A_ROW.bind([Value::Uuid(Some(job.into_inner()))]))
        .await?
        .ok_or_else(|| DbErr::Custom("RUNNER_LOSSES_IN_A_ROW returned no row".to_owned()))?
        .try_get::<i64>("", "losses")
}

pub async fn open_of_job<C: ConnectionTrait>(
    db: &C,
    job: RunnerJobId,
) -> Result<Option<MRunnerJobAttempt>, DbErr> {
    ERunnerJobAttempt::find()
        .filter(CRunnerJobAttempt::RunnerJob.eq(job))
        .filter(CRunnerJobAttempt::EndedAt.is_null())
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use sea_orm::MockExecResult;

    #[tokio::test]
    async fn a_started_job_drops_its_wait_reason() {
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let opened = open(
            &db,
            RunnerJobId::now_v7(),
            RunnerJobAttemptId::now_v7(),
            1,
            DispatchedJobId::now_v7(),
            "w1",
            2,
        )
        .await
        .unwrap();

        assert!(!opened);
        let log = all_statements(db.into_transaction_log());
        assert!(
            log[0].sql.contains("waiting_reason = NULL"),
            "{}",
            log[0].sql
        );
    }
}
