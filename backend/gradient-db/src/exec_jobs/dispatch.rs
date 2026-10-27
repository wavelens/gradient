/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::store_path::StorePath;
use gradient_types::exec::Phase;
use gradient_types::{ExecJobAttemptId, ExecJobId, MExecJob, ProjectId, TaskId, WaitingReason};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, QueryResult, Value};
use std::sync::LazyLock;

use crate::scheduling::assignment_record::{exec_job_key_sql, no_open_assignment_predicate};
use crate::sql::status::{exec, exec_in};

#[derive(Debug, Clone)]
pub struct ExecDispatchRow {
    pub job: MExecJob,
    pub entry_point: String,
    pub task: TaskId,
    pub project: ProjectId,
    pub repository: String,
    pub commit: String,
    pub architecture: String,
    pub image_path: Option<String>,
    pub attempts_done: i32,
    pub last_phase: Option<Phase>,
    pub open_attempt: Option<OpenAttempt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAttempt {
    pub id: ExecJobAttemptId,
    pub number: i32,
}

fn dispatch_rows_sql(filter: &str) -> String {
    format!(
        "SELECT r.*, e.eval AS entry_point_name, ev.task AS task_id, t.project AS project_id, \
         ev.repository, encode(c.hash, 'hex') AS commit_hash, d.architecture, \
         img.hash AS image_hash, img.package AS image_package, \
         (SELECT count(*) FROM exec_job_attempt a WHERE a.exec_job = r.id)::integer \
              AS attempts_done, \
         oa.id AS open_attempt_id, oa.number AS open_attempt_number, \
         lp.name AS last_phase_name, lp.payload AS last_phase_payload \
         FROM exec_job r \
         JOIN entry_point e ON e.id = r.entry_point \
         JOIN evaluation ev ON ev.id = r.evaluation \
         JOIN task t ON t.id = ev.task \
         JOIN commit c ON c.id = ev.commit \
         JOIN derivation d ON d.id = r.program_derivation \
         LEFT JOIN derivation_output img ON img.derivation = r.image_derivation AND img.name = 'out' \
         LEFT JOIN exec_job_attempt oa ON oa.exec_job = r.id AND oa.ended_at IS NULL \
         LEFT JOIN LATERAL (SELECT p.name, p.payload FROM exec_job_phase p \
              WHERE p.exec_job = r.id ORDER BY p.created_at DESC LIMIT 1) lp ON true \
         WHERE {filter} \
         ORDER BY r.created_at"
    )
}

static QUEUED_UNTRACKED_SQL: LazyLock<String> = LazyLock::new(|| {
    dispatch_rows_sql(&format!(
        "r.status = {queued} AND {untracked}",
        queued = exec(ExecJobStatus::Queued),
        untracked = no_open_assignment_predicate(&exec_job_key_sql("r.id")),
    ))
});

static STARTED_SQL: LazyLock<String> = LazyLock::new(|| {
    dispatch_rows_sql(&format!(
        "r.id = ANY($1::uuid[]) AND r.status IN ({started})",
        started = exec_in(&[ExecJobStatus::Starting, ExecJobStatus::Running]),
    ))
});

crate::sql_lazy! {
    QUEUED_UNTRACKED_EXEC_JOBS = || QUEUED_UNTRACKED_SQL.as_str(),
        params = [];
    STARTED_EXEC_JOBS = || STARTED_SQL.as_str(),
        params = [NewUuids(16)];
}

crate::sql! {
    SET_EXEC_JOB_WAITING_REASON = "UPDATE exec_job SET waiting_reason = $2::jsonb \
         WHERE id = $1 AND waiting_reason IS DISTINCT FROM $2::jsonb",
        params = [NewUuid, Text("{\"kind\":\"draining\"}")];
}

pub async fn queued_untracked<C: ConnectionTrait>(db: &C) -> Result<Vec<ExecDispatchRow>, DbErr> {
    db.query_all_raw(QUEUED_UNTRACKED_EXEC_JOBS.stmt())
        .await?
        .iter()
        .map(dispatch_row)
        .collect()
}

pub async fn started<C: ConnectionTrait>(
    db: &C,
    jobs: &[ExecJobId],
) -> Result<Vec<ExecDispatchRow>, DbErr> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = jobs.iter().map(|id| id.into_inner()).collect();
    db.query_all_raw(STARTED_EXEC_JOBS.bind([ids.into()]))
        .await?
        .iter()
        .map(dispatch_row)
        .collect()
}

fn dispatch_row(row: &QueryResult) -> Result<ExecDispatchRow, DbErr> {
    let image_hash: Option<String> = row.try_get("", "image_hash")?;
    let image_package: Option<String> = row.try_get("", "image_package")?;
    let phase_name: Option<String> = row.try_get("", "last_phase_name")?;
    let phase_payload: Option<Vec<u8>> = row.try_get("", "last_phase_payload")?;
    let open_attempt_id: Option<ExecJobAttemptId> = row.try_get("", "open_attempt_id")?;
    let open_attempt_number: Option<i32> = row.try_get("", "open_attempt_number")?;
    Ok(ExecDispatchRow {
        job: MExecJob::from_query_result(row, "")?,
        entry_point: row.try_get("", "entry_point_name")?,
        task: row.try_get("", "task_id")?,
        project: row.try_get("", "project_id")?,
        repository: row.try_get("", "repository")?,
        commit: row.try_get("", "commit_hash")?,
        architecture: row.try_get("", "architecture")?,
        image_path: image_hash
            .zip(image_package)
            .map(|(hash, package)| StorePath::from_stored(hash, package).full()),
        attempts_done: row.try_get("", "attempts_done")?,
        last_phase: phase_name.map(|name| Phase {
            name,
            payload: phase_payload.unwrap_or_default(),
        }),
        open_attempt: open_attempt_id
            .zip(open_attempt_number)
            .map(|(id, number)| OpenAttempt { id, number }),
    })
}

pub async fn set_waiting_reason<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    reason: Option<&WaitingReason>,
) -> Result<(), DbErr> {
    db.execute_raw(SET_EXEC_JOB_WAITING_REASON.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::Json(reason.map(|r| Box::new(r.to_json()))),
    ]))
    .await?;
    Ok(())
}
