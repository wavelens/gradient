/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::runner_job::RunnerJobStatus;
use gradient_entity::store_path::StorePath;
use gradient_types::runner::Phase;
use gradient_types::{MRunnerJob, ProjectId, RunnerJobId, TaskId, WaitingReason};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, QueryResult, Value};
use std::sync::LazyLock;

use crate::scheduling::assignment_record::{no_open_assignment_predicate, runner_job_key_sql};
use crate::sql::status::runner;

#[derive(Debug, Clone)]
pub struct RunnerDispatchRow {
    pub job: MRunnerJob,
    pub entry_point: String,
    pub task: TaskId,
    pub project: ProjectId,
    pub repository: String,
    pub commit: String,
    pub architecture: String,
    pub image_path: Option<String>,
    pub attempts_done: i32,
    pub last_phase: Option<Phase>,
}

static QUEUED_UNTRACKED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT r.*, e.eval AS entry_point_name, ev.task AS task_id, t.project AS project_id, \
         ev.repository, encode(c.hash, 'hex') AS commit_hash, d.architecture, \
         img.hash AS image_hash, img.package AS image_package, \
         (SELECT count(*) FROM runner_job_attempt a WHERE a.runner_job = r.id)::integer \
              AS attempts_done, \
         lp.name AS last_phase_name, lp.payload AS last_phase_payload \
         FROM runner_job r \
         JOIN entry_point e ON e.id = r.entry_point \
         JOIN evaluation ev ON ev.id = r.evaluation \
         JOIN task t ON t.id = ev.task \
         JOIN commit c ON c.id = ev.commit \
         JOIN derivation d ON d.id = r.program_derivation \
         LEFT JOIN derivation_output img ON img.derivation = r.image_derivation AND img.name = 'out' \
         LEFT JOIN LATERAL (SELECT p.name, p.payload FROM runner_job_phase p \
              WHERE p.runner_job = r.id ORDER BY p.created_at DESC LIMIT 1) lp ON true \
         WHERE r.status = {queued} AND {untracked} \
         ORDER BY r.created_at",
        queued = runner(RunnerJobStatus::Queued),
        untracked = no_open_assignment_predicate(&runner_job_key_sql("r.id")),
    )
});

crate::sql_lazy! {
    QUEUED_UNTRACKED_RUNNER_JOBS = || QUEUED_UNTRACKED_SQL.as_str(),
        params = [];
}

crate::sql! {
    SET_RUNNER_JOB_WAITING_REASON = "UPDATE runner_job SET waiting_reason = $2::jsonb \
         WHERE id = $1 AND waiting_reason IS DISTINCT FROM $2::jsonb",
        params = [NewUuid, Text("{\"kind\":\"draining\"}")];
}

pub async fn queued_untracked<C: ConnectionTrait>(db: &C) -> Result<Vec<RunnerDispatchRow>, DbErr> {
    db.query_all_raw(QUEUED_UNTRACKED_RUNNER_JOBS.stmt())
        .await?
        .iter()
        .map(dispatch_row)
        .collect()
}

fn dispatch_row(row: &QueryResult) -> Result<RunnerDispatchRow, DbErr> {
    let image_hash: Option<String> = row.try_get("", "image_hash")?;
    let image_package: Option<String> = row.try_get("", "image_package")?;
    let phase_name: Option<String> = row.try_get("", "last_phase_name")?;
    let phase_payload: Option<Vec<u8>> = row.try_get("", "last_phase_payload")?;
    Ok(RunnerDispatchRow {
        job: MRunnerJob::from_query_result(row, "")?,
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
    })
}

pub async fn set_waiting_reason<C: ConnectionTrait>(
    db: &C,
    job: RunnerJobId,
    reason: Option<&WaitingReason>,
) -> Result<(), DbErr> {
    db.execute_raw(SET_RUNNER_JOB_WAITING_REASON.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::Json(reason.map(|r| Box::new(r.to_json()))),
    ]))
    .await?;
    Ok(())
}
