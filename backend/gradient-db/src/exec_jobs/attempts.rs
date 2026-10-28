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
         retries_left = LEAST(retries_left, $2) \
         WHERE id = $1 AND status = {queued} \
         AND NOT EXISTS (SELECT 1 FROM exec_job_attempt a \
              WHERE a.exec_job = $1 AND a.number >= $3)",
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
         finished_at = (now() AT TIME ZONE 'UTC') \
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

pub async fn losses_in_a_row<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<i64, DbErr> {
    EXEC_LOSSES_IN_A_ROW
        .scalar(db, [job.into()], "losses")
        .await
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
            None,
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

    #[tokio::test]
    async fn closing_an_attempt_revokes_its_tokens() {
        let attempt = ExecJobAttemptId::now_v7();
        let job = MExecJob {
            id: ExecJobId::now_v7(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![MExecJobAttempt {
                id: attempt,
                exec_job: job.id,
                ..Default::default()
            }]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![job]])
            .into_connection();

        close(&db, attempt, AttemptEndReason::Completed, None)
            .await
            .unwrap()
            .expect("the attempt closes");

        let log = all_statements(db.into_transaction_log());
        assert!(
            log.iter().any(|s| s
                .sql
                .starts_with("UPDATE exec_job_token SET revoked = true WHERE attempt = $1")),
            "{log:?}"
        );
    }
}
