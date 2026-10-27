/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::{
    CExecJobAttempt, DispatchedJobId, EExecJob, EExecJobAttempt, ExecJobAttemptId, ExecJobId,
    MExecJob, MExecJobAttempt,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, Value,
};
use std::sync::LazyLock;

use crate::sql::status::{exec, exec_in};

static OPEN_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {starting}, waiting_reason = NULL, \
         retries_left = LEAST(retries_left, $2) \
         WHERE id = $1 AND status = {queued}",
        starting = exec(ExecJobStatus::Starting),
        queued = exec(ExecJobStatus::Queued),
    )
});

static PROGRAM_STARTED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job r SET status = {running}, \
         started_at = COALESCE(r.started_at, (now() AT TIME ZONE 'UTC')) \
         FROM exec_job_attempt a \
         WHERE a.id = $1 AND a.ended_at IS NULL AND r.id = a.exec_job AND r.status = {starting} \
         RETURNING r.*",
        running = exec(ExecJobStatus::Running),
        starting = exec(ExecJobStatus::Starting),
    )
});

static LOSSES_IN_A_ROW_SQL: LazyLock<String> = LazyLock::new(|| {
    let lost = i16::from(AttemptEndReason::Lost);
    format!(
        "SELECT count(*) AS losses FROM exec_job_attempt a \
         WHERE a.exec_job = $1 AND a.end_reason = {lost} \
         AND a.number > COALESCE((SELECT max(b.number) FROM exec_job_attempt b \
              WHERE b.exec_job = $1 AND b.end_reason <> {lost}), 0)"
    )
});

static REQUEUE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {queued}, waiting_reason = NULL, \
         retries_left = GREATEST(retries_left - $2, 0) \
         WHERE id = $1 AND status IN ({started}) RETURNING *",
        queued = exec(ExecJobStatus::Queued),
        started = exec_in(&[ExecJobStatus::Starting, ExecJobStatus::Running]),
    )
});

static FINISH_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = $2, message = $3, waiting_reason = NULL, \
         finished_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = $1 AND status IN ({open}) RETURNING *",
        open = exec_in(&ExecJobStatus::OPEN),
    )
});

crate::sql_lazy! {
    OPEN_EXEC_JOB_ATTEMPT = || OPEN_SQL.as_str(),
        params = [NewUuid, Int(3)];
    REQUEUE_EXEC_JOB = || REQUEUE_SQL.as_str(),
        params = [NewUuid, Int(1)];
    FINISH_EXEC_JOB = || FINISH_SQL.as_str(),
        params = [NewUuid, Int(4), Text("the program exited with 1")];
    EXEC_PROGRAM_STARTED = || PROGRAM_STARTED_SQL.as_str(),
        params = [NewUuid];
    EXEC_LOSSES_IN_A_ROW = || LOSSES_IN_A_ROW_SQL.as_str(),
        params = [NewUuid];
}

crate::sql! {
    CLOSE_EXEC_JOB_ATTEMPT = "UPDATE exec_job_attempt \
         SET ended_at = (now() AT TIME ZONE 'UTC'), end_reason = $2, message = $3 \
         WHERE id = $1 AND ended_at IS NULL RETURNING *",
        params = [NewUuid, Int(3), Text("lost")];
}

pub async fn open<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    attempt: ExecJobAttemptId,
    number: i32,
    assignment: DispatchedJobId,
    worker: &str,
    retries_cap: i32,
) -> Result<bool, DbErr> {
    let started = db
        .execute_raw(OPEN_EXEC_JOB_ATTEMPT.bind([
            Value::Uuid(Some(job.into_inner())),
            Value::Int(Some(retries_cap)),
        ]))
        .await?;
    if started.rows_affected() != 1 {
        return Ok(false);
    }

    crate::rows::insert::<EExecJobAttempt, _>(
        db,
        MExecJobAttempt {
            id: attempt,
            exec_job: job,
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
    attempt: ExecJobAttemptId,
) -> Result<Option<MExecJob>, DbErr> {
    MExecJob::find_by_statement(
        EXEC_PROGRAM_STARTED.bind([Value::Uuid(Some(attempt.into_inner()))]),
    )
    .one(db)
    .await
}

pub async fn close<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    reason: AttemptEndReason,
    message: Option<String>,
) -> Result<Option<(MExecJob, MExecJobAttempt)>, DbErr> {
    let Some(closed) = MExecJobAttempt::find_by_statement(CLOSE_EXEC_JOB_ATTEMPT.bind([
        Value::Uuid(Some(attempt.into_inner())),
        Value::SmallInt(Some(i16::from(reason))),
        Value::String(message),
    ]))
    .one(db)
    .await?
    else {
        return Ok(None);
    };
    let job = EExecJob::find_by_id(closed.exec_job)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("exec job {}", closed.exec_job)))?;
    Ok(Some((job, closed)))
}

pub async fn requeue<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    consume_retry: bool,
) -> Result<Option<MExecJob>, DbErr> {
    MExecJob::find_by_statement(REQUEUE_EXEC_JOB.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::Int(Some(i32::from(consume_retry))),
    ]))
    .one(db)
    .await
}

pub async fn finish<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    status: ExecJobStatus,
    message: Option<String>,
) -> Result<Option<MExecJob>, DbErr> {
    MExecJob::find_by_statement(FINISH_EXEC_JOB.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::SmallInt(Some(i16::from(status))),
        Value::String(message),
    ]))
    .one(db)
    .await
}

pub async fn losses_in_a_row<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<i64, DbErr> {
    db.query_one_raw(EXEC_LOSSES_IN_A_ROW.bind([Value::Uuid(Some(job.into_inner()))]))
        .await?
        .ok_or_else(|| DbErr::Custom("EXEC_LOSSES_IN_A_ROW returned no row".to_owned()))?
        .try_get::<i64>("", "losses")
}

pub async fn open_of_job<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Option<MExecJobAttempt>, DbErr> {
    EExecJobAttempt::find()
        .filter(CExecJobAttempt::ExecJob.eq(job))
        .filter(CExecJobAttempt::EndedAt.is_null())
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
            ExecJobId::now_v7(),
            ExecJobAttemptId::now_v7(),
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
