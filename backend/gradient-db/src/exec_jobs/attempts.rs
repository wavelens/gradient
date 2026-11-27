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

use crate::sql::StatusWrite;
use crate::sql::status::{exec, exec_in};

static OPEN_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {starting}, waiting_reason = NULL, \
         program_exit_code = NULL, retries_left = LEAST(retries_left, $2) \
         WHERE id = $1 AND status = {queued} \
         AND NOT EXISTS (SELECT 1 FROM exec_job_attempt a \
              WHERE a.exec_job = $1 AND a.number >= $3)",
        starting = exec(ExecJobStatus::Starting),
        queued = exec(ExecJobStatus::Queued),
    )
});

static PROGRAM_STARTED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "WITH a AS (UPDATE exec_job_attempt \
              SET program_started_at = COALESCE(program_started_at, (now() AT TIME ZONE 'UTC')) \
              WHERE id = $1 AND ended_at IS NULL RETURNING exec_job) \
         UPDATE exec_job r SET status = {running}, \
         started_at = COALESCE(r.started_at, (now() AT TIME ZONE 'UTC')) \
         FROM a WHERE r.id = a.exec_job AND r.status = {starting} \
         RETURNING r.*",
        running = exec(ExecJobStatus::Running),
        starting = exec(ExecJobStatus::Starting),
    )
});

static PROGRAM_EXITED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job r SET program_exit_code = $2 \
         FROM exec_job_attempt a \
         WHERE a.id = $1 AND a.ended_at IS NULL AND r.id = a.exec_job AND r.status = {running} \
         RETURNING r.*",
        running = exec(ExecJobStatus::Running),
    )
});

static LOSSES_IN_A_ROW_SQL: LazyLock<String> = LazyLock::new(|| {
    let lost = i16::from(AttemptEndReason::Lost);
    format!(
        "SELECT count(*) AS losses, \
         CASE WHEN count(a.message) = count(*) AND min(a.message) = max(a.message) \
              THEN min(a.message) END AS shared_loss_failure_text \
         FROM exec_job_attempt a \
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

static REQUEUE_NODES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {queued}, waiting_reason = NULL, \
         retries_left = GREATEST(retries_left - $2, 0) \
         WHERE entry_point = (SELECT entry_point FROM exec_job WHERE id = $1) \
         AND status IN ({placed}) RETURNING *",
        queued = exec(ExecJobStatus::Queued),
        placed = exec_in(&[
            ExecJobStatus::Queued,
            ExecJobStatus::Starting,
            ExecJobStatus::Running
        ]),
    )
});

fn finish_sql(status: ExecJobStatus) -> String {
    format!(
        "UPDATE exec_job SET status = {to}, message = $2, waiting_reason = NULL, \
         keep_vm_running = false, finished_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = $1 AND status IN ({open}) RETURNING *",
        to = exec(status),
        open = exec_in(&ExecJobStatus::OPEN),
    )
}

static COMPLETE_SQL: LazyLock<String> = LazyLock::new(|| finish_sql(ExecJobStatus::Completed));
static FAIL_SQL: LazyLock<String> = LazyLock::new(|| finish_sql(ExecJobStatus::Failed));
static ABORT_SQL: LazyLock<String> = LazyLock::new(|| finish_sql(ExecJobStatus::Aborted));

crate::sql_lazy! {
    OPEN_EXEC_JOB_ATTEMPT = || OPEN_SQL.as_str(),
        params = [NewUuid, Int(3), Int(1)],
        status_write = StatusWrite::exec_job(ExecJobStatus::Starting);
    REQUEUE_EXEC_JOB = || REQUEUE_SQL.as_str(),
        params = [NewUuid, Int(1)],
        status_write = StatusWrite::exec_job(ExecJobStatus::Queued);
    REQUEUE_EXEC_JOB_NODES = || REQUEUE_NODES_SQL.as_str(),
        params = [NewUuid, Int(1)],
        status_write = StatusWrite::exec_job(ExecJobStatus::Queued);
    COMPLETE_EXEC_JOB = || COMPLETE_SQL.as_str(),
        params = [NewUuid, Text("")],
        status_write = StatusWrite::exec_job(ExecJobStatus::Completed);
    FAIL_EXEC_JOB = || FAIL_SQL.as_str(),
        params = [NewUuid, Text("the program exited with 1")],
        status_write = StatusWrite::exec_job(ExecJobStatus::Failed);
    ABORT_EXEC_JOB = || ABORT_SQL.as_str(),
        params = [NewUuid, Text("")],
        status_write = StatusWrite::exec_job(ExecJobStatus::Aborted);
    EXEC_PROGRAM_STARTED = || PROGRAM_STARTED_SQL.as_str(),
        params = [NewUuid],
        status_write = StatusWrite::exec_job(ExecJobStatus::Running);
    EXEC_LOSSES_IN_A_ROW = || LOSSES_IN_A_ROW_SQL.as_str(),
        params = [NewUuid];
    EXEC_PROGRAM_EXITED = || PROGRAM_EXITED_SQL.as_str(),
        params = [NewUuid, Int(3)];
}

crate::sql! {
    CLOSE_EXEC_JOB_ATTEMPT = "UPDATE exec_job_attempt \
         SET ended_at = (now() AT TIME ZONE 'UTC'), end_reason = $2, message = $3 \
         WHERE id = $1 AND ended_at IS NULL RETURNING *",
        params = [NewUuid, Int(3), Text("lost")];
    REVOKE_EXEC_JOB_TOKENS_OF_ATTEMPT = "UPDATE exec_job_token SET revoked = true \
         WHERE attempt = $1 AND NOT revoked",
        params = [NewUuid];
    SET_EXEC_JOB_ATTEMPT_GUEST_ADDRESS = "UPDATE exec_job_attempt SET guest_address = $2 \
         WHERE id = $1",
        params = [NewUuid, Text("10.233.0.2")];
}

#[expect(
    clippy::too_many_arguments,
    reason = "an attempt row is written from the fields of one assignment"
)]
pub async fn open<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    attempt: ExecJobAttemptId,
    number: i32,
    assignment: DispatchedJobId,
    worker: &str,
    retries_cap: i32,
    master_port: Option<u16>,
) -> Result<bool, DbErr> {
    let started = db
        .execute_raw(OPEN_EXEC_JOB_ATTEMPT.bind([
            Value::Uuid(Some(job.into_inner())),
            Value::Int(Some(retries_cap)),
            Value::Int(Some(number)),
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
            master_port: master_port.map(i32::from),
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

pub async fn program_exited<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    code: i32,
) -> Result<Option<MExecJob>, DbErr> {
    MExecJob::find_by_statement(EXEC_PROGRAM_EXITED.bind([
        Value::Uuid(Some(attempt.into_inner())),
        Value::Int(Some(code)),
    ]))
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
    db.execute_raw(
        REVOKE_EXEC_JOB_TOKENS_OF_ATTEMPT.bind([Value::Uuid(Some(attempt.into_inner()))]),
    )
    .await?;
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

pub async fn requeue_nodes<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    consume_retry: bool,
) -> Result<Vec<MExecJob>, DbErr> {
    MExecJob::find_by_statement(REQUEUE_EXEC_JOB_NODES.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::Int(Some(i32::from(consume_retry))),
    ]))
    .all(db)
    .await
}

pub async fn finish<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    status: ExecJobStatus,
    message: Option<String>,
) -> Result<Option<MExecJob>, DbErr> {
    let statement = match status {
        ExecJobStatus::Completed => &COMPLETE_EXEC_JOB,
        ExecJobStatus::Failed => &FAIL_EXEC_JOB,
        ExecJobStatus::Aborted => &ABORT_EXEC_JOB,
        open => {
            return Err(DbErr::Custom(format!(
                "{open:?} does not finish an exec job"
            )));
        }
    };
    MExecJob::find_by_statement(
        statement.bind([Value::Uuid(Some(job.into_inner())), Value::String(message)]),
    )
    .one(db)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct ConsecutiveLostWorkerAttemptsWithSharedFailureText {
    pub losses: i64,
    pub shared_loss_failure_text: Option<String>,
}

pub async fn get_latest_consecutive_attempts_that_lost_worker_count_and_shared_failure_text_for_exec_job<
    C: ConnectionTrait,
>(
    db: &C,
    job: ExecJobId,
) -> Result<ConsecutiveLostWorkerAttemptsWithSharedFailureText, DbErr> {
    ConsecutiveLostWorkerAttemptsWithSharedFailureText::find_by_statement(
        EXEC_LOSSES_IN_A_ROW.bind([job.into()]),
    )
    .one(db)
    .await?
    .ok_or_else(|| DbErr::Custom("loss count returned no row".into()))
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

pub async fn set_guest_address<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    address: &str,
) -> Result<(), DbErr> {
    db.execute_raw(SET_EXEC_JOB_ATTEMPT_GUEST_ADDRESS.bind([
        Value::Uuid(Some(attempt.into_inner())),
        Value::String(Some(address.to_owned())),
    ]))
    .await?;
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_types::EvaluationId;

    pub(in crate::exec_jobs) async fn two_queued_jobs() -> (TestDatabase, ExecJobId, ExecJobId) {
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("{}-first.drv", "a".repeat(32)))
            .entry_point("first")
            .derivation(&format!("{}-second.drv", "b".repeat(32)))
            .entry_point("second")
            .into_database()
            .await;
        db.execute(
            "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, requirement, \
             spec, retries_left, status, waiting_reason, created_at) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, '{}'::jsonb, '{}'::jsonb, \
             3, 0, '{\"kind\":\"draining\"}'::jsonb, now() AT TIME ZONE 'UTC' FROM entry_point e",
        )
        .await;
        let job_of = |attr: &'static str| {
            let db = &db;
            async move {
                db.texts(&format!(
                    "SELECT j.id::text FROM exec_job j JOIN entry_point e ON e.id = j.entry_point \
                     WHERE e.eval = '{attr}'"
                ))
                .await[0]
                    .parse::<uuid::Uuid>()
                    .map(ExecJobId::new)
                    .unwrap()
            }
        };
        let (first, second) = (job_of("first").await, job_of("second").await);
        (db, first, second)
    }

    #[tokio::test]
    async fn opening_attempt_drops_wait_reason_refuses_reopen_and_closing_revokes_only_its_tokens()
    {
        let (db, first, second) = two_queued_jobs().await;
        let conn = db.connection();
        let (a1, a2) = (ExecJobAttemptId::now_v7(), ExecJobAttemptId::now_v7());
        let open_attempt = |job, attempt| {
            open(
                &conn,
                job,
                attempt,
                1,
                DispatchedJobId::now_v7(),
                "w1",
                2,
                None,
            )
        };

        let opened = open_attempt(first, a1).await.unwrap();
        let reopened = open_attempt(first, ExecJobAttemptId::now_v7())
            .await
            .unwrap();
        assert!(open_attempt(second, a2).await.unwrap());
        let expires = gradient_types::now() + chrono::Duration::hours(1);
        assert!(
            super::super::tokens::insert(&conn, a1, "first", expires)
                .await
                .unwrap()
        );
        assert!(
            super::super::tokens::insert(&conn, a2, "second", expires)
                .await
                .unwrap()
        );
        let closed = close(&conn, a1, AttemptEndReason::Completed, None)
            .await
            .unwrap();
        let closed_again = close(&conn, a1, AttemptEndReason::Completed, None)
            .await
            .unwrap();

        let first_job = db.find::<EExecJob>(first).await.unwrap();
        let tokens_of = |job| super::super::tokens::of_job(&conn, job);
        assert!(opened && !reopened);
        assert_eq!(
            (first_job.status, first_job.waiting_reason),
            (ExecJobStatus::Starting, None)
        );
        assert_eq!(closed.map(|(job, _)| job.id), Some(first));
        assert!(closed_again.is_none());
        assert_eq!(
            tokens_of(first)
                .await
                .unwrap()
                .iter()
                .map(|t| t.revoked)
                .collect::<Vec<_>>(),
            [true]
        );
        assert_eq!(
            tokens_of(second)
                .await
                .unwrap()
                .iter()
                .map(|t| t.revoked)
                .collect::<Vec<_>>(),
            [false]
        );
        db.finish().await;
    }
}
