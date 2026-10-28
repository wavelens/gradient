/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_continuation::ContinuationStatus;
use gradient_types::{EvaluationId, ExecJobId, MEvaluation, MExecJobContinuation, ProjectId};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, QueryResult, Value};
use std::sync::LazyLock;

use crate::scheduling::assignment_record::{
    continuation_job_key_sql, no_open_assignment_predicate,
};
use crate::sql::status::{eval_in, exec_in};

#[derive(Debug, Clone)]
pub struct ContinuationRow {
    pub job: ExecJobId,
    pub evaluation: MEvaluation,
    pub commit_sha: String,
    pub attributes: Vec<String>,
    pub project: ProjectId,
}

fn code(status: ContinuationStatus) -> i16 {
    i16::from(status)
}

static RECORD_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "INSERT INTO exec_job_continuation \
         (exec_job, evaluation, attributes, status, created_at, updated_at) \
         SELECT j.id, $2::uuid, $3::jsonb, {recorded}, (now() AT TIME ZONE 'UTC'), \
              (now() AT TIME ZONE 'UTC') \
         FROM exec_job j WHERE j.id = $1 AND j.status IN ({open}) \
         ON CONFLICT (exec_job) DO UPDATE SET attributes = EXCLUDED.attributes, \
              updated_at = EXCLUDED.updated_at \
         WHERE exec_job_continuation.status = {recorded}",
        recorded = code(ContinuationStatus::Recorded),
        open = exec_in(&ExecJobStatus::OPEN),
    )
});

static FORGET_RECORDED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "DELETE FROM exec_job_continuation WHERE exec_job = $1 AND status = {recorded}",
        recorded = code(ContinuationStatus::Recorded),
    )
});

static ACTIVATE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job_continuation SET status = {queued}, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE exec_job = $1 AND status = {recorded}",
        queued = code(ContinuationStatus::Queued),
        recorded = code(ContinuationStatus::Recorded),
    )
});

static HAS_OPEN_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM exec_job_continuation \
         WHERE evaluation = $1 AND status = {queued}) AS open",
        queued = code(ContinuationStatus::Queued),
    )
});

static IS_QUEUED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM exec_job_continuation \
         WHERE exec_job = $1 AND status = {queued}) AS queued",
        queued = code(ContinuationStatus::Queued),
    )
});

static QUEUED_UNTRACKED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT ev.*, c.exec_job AS continuation_job, c.attributes AS continuation_attributes, \
         encode(cm.hash, 'hex') AS commit_hash, t.project AS project_id \
         FROM exec_job_continuation c \
         JOIN evaluation ev ON ev.id = c.evaluation \
         JOIN commit cm ON cm.id = ev.commit \
         JOIN task t ON t.id = ev.task \
         WHERE c.status = {queued} AND ev.status NOT IN ({terminal}) AND {untracked} \
         ORDER BY c.updated_at",
        queued = code(ContinuationStatus::Queued),
        terminal = eval_in(&[
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ]),
        untracked = no_open_assignment_predicate(&continuation_job_key_sql("c.exec_job")),
    )
});

static END_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job_continuation SET status = $2, message = $3, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE exec_job = $1 AND status = {queued} RETURNING *",
        queued = code(ContinuationStatus::Queued),
    )
});

static FAIL_OPEN_OF_EVALUATION_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job_continuation SET status = {failed}, \
         message = 'the evaluation was aborted', updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE evaluation = $1 AND status IN ({recorded}, {queued}) RETURNING exec_job",
        failed = code(ContinuationStatus::Failed),
        recorded = code(ContinuationStatus::Recorded),
        queued = code(ContinuationStatus::Queued),
    )
});

crate::sql_lazy! {
    RECORD_EXEC_JOB_CONTINUATION = || RECORD_SQL.as_str(),
        params = [NewUuid, EvaluationId, Text("[\"packages.x86_64-linux.a\"]")];
    FORGET_RECORDED_EXEC_JOB_CONTINUATION = || FORGET_RECORDED_SQL.as_str(),
        params = [NewUuid];
    ACTIVATE_EXEC_JOB_CONTINUATION = || ACTIVATE_SQL.as_str(),
        params = [NewUuid];
    HAS_OPEN_EXEC_JOB_CONTINUATION = || HAS_OPEN_SQL.as_str(),
        params = [EvaluationId];
    IS_EXEC_JOB_CONTINUATION_QUEUED = || IS_QUEUED_SQL.as_str(),
        params = [NewUuid];
    QUEUED_UNTRACKED_EXEC_JOB_CONTINUATIONS = || QUEUED_UNTRACKED_SQL.as_str(),
        params = [];
    END_EXEC_JOB_CONTINUATION = || END_SQL.as_str(),
        params = [NewUuid, Int(2), Text("")];
    FAIL_OPEN_EXEC_JOB_CONTINUATIONS_OF_EVALUATION = || FAIL_OPEN_OF_EVALUATION_SQL.as_str(),
        params = [EvaluationId];
}

fn job_value(job: ExecJobId) -> Value {
    Value::Uuid(Some(job.into_inner()))
}

pub async fn record<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    evaluation: EvaluationId,
    attributes: &[String],
) -> Result<bool, DbErr> {
    let recorded = db
        .execute_raw(RECORD_EXEC_JOB_CONTINUATION.bind([
            job_value(job),
            Value::Uuid(Some(evaluation.into_inner())),
            Value::Json(Some(Box::new(serde_json::json!(attributes)))),
        ]))
        .await?;
    Ok(recorded.rows_affected() == 1)
}

pub async fn forget_recorded<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<(), DbErr> {
    db.execute_raw(FORGET_RECORDED_EXEC_JOB_CONTINUATION.bind([job_value(job)]))
        .await?;
    Ok(())
}

pub async fn activate<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<bool, DbErr> {
    let activated = db
        .execute_raw(ACTIVATE_EXEC_JOB_CONTINUATION.bind([job_value(job)]))
        .await?;
    Ok(activated.rows_affected() == 1)
}

pub async fn has_open<C: ConnectionTrait>(db: &C, evaluation: EvaluationId) -> Result<bool, DbErr> {
    HAS_OPEN_EXEC_JOB_CONTINUATION
        .scalar(db, [evaluation.into()], "open")
        .await
}

pub async fn is_queued<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<bool, DbErr> {
    IS_EXEC_JOB_CONTINUATION_QUEUED
        .scalar(db, [job_value(job)], "queued")
        .await
}

pub async fn queued_untracked<C: ConnectionTrait>(db: &C) -> Result<Vec<ContinuationRow>, DbErr> {
    db.query_all_raw(QUEUED_UNTRACKED_EXEC_JOB_CONTINUATIONS.stmt())
        .await?
        .iter()
        .map(continuation_row)
        .collect()
}

fn continuation_row(row: &QueryResult) -> Result<ContinuationRow, DbErr> {
    let attributes: serde_json::Value = row.try_get("", "continuation_attributes")?;
    Ok(ContinuationRow {
        job: row.try_get("", "continuation_job")?,
        evaluation: MEvaluation::from_query_result(row, "")?,
        commit_sha: row.try_get("", "commit_hash")?,
        attributes: serde_json::from_value(attributes)
            .map_err(|e| DbErr::Json(format!("continuation attributes: {e}")))?,
        project: row.try_get("", "project_id")?,
    })
}

pub async fn end<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    status: ContinuationStatus,
    message: Option<String>,
) -> Result<Option<MExecJobContinuation>, DbErr> {
    if !matches!(
        status,
        ContinuationStatus::Done | ContinuationStatus::Failed
    ) {
        return Err(DbErr::Custom(format!(
            "{status:?} does not end a continuation"
        )));
    }
    MExecJobContinuation::find_by_statement(END_EXEC_JOB_CONTINUATION.bind([
        job_value(job),
        Value::SmallInt(Some(code(status))),
        Value::String(message),
    ]))
    .one(db)
    .await
}

pub async fn fail_open_of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<ExecJobId>, DbErr> {
    db.query_all_raw(
        FAIL_OPEN_EXEC_JOB_CONTINUATIONS_OF_EVALUATION
            .bind([Value::Uuid(Some(evaluation.into_inner()))]),
    )
    .await?
    .iter()
    .map(|row| row.try_get("", "exec_job"))
    .collect()
}
