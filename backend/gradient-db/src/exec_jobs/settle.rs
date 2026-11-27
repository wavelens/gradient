/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{ExecJobId, MExecJob};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Value};
use std::sync::LazyLock;

use crate::build_request_task::{build_request_task_sql, task_exec_switch_sql};
use crate::sql::StatusWrite;
use crate::sql::status::{build_in, exec, exec_in};
use gradient_types::exec::{DISABLED_FOR_BUILD_REQUESTS, DISABLED_FOR_TASK, DISABLED_ON_INSTANCE};

#[derive(Debug, Default)]
pub struct Settled {
    pub queued: Vec<MExecJob>,
    pub failed: Vec<MExecJob>,
}

pub(crate) fn build_in_state(derivation: &str, statuses: &[BuildStatus]) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation_build b WHERE b.derivation = {derivation} \
         AND b.status IN ({}))",
        build_in(statuses),
    )
}

static QUEUE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job r SET status = {queued}, waiting_reason = NULL \
         WHERE r.status = {created} AND NOT r.waits_for_manual_start AND {program_done} \
         AND (r.image_derivation IS NULL OR {image_done}) \
         RETURNING r.*",
        queued = exec(ExecJobStatus::Queued),
        created = exec(ExecJobStatus::Created),
        program_done = build_in_state("r.program_derivation", &BuildStatus::TERMINAL_SUCCESS),
        image_done = build_in_state("r.image_derivation", &BuildStatus::TERMINAL_SUCCESS),
    )
});

static FAIL_SQL: LazyLock<String> = LazyLock::new(|| {
    let program_failed = build_in_state("r.program_derivation", &BuildStatus::REQUEUEABLE);
    let image_failed = build_in_state("r.image_derivation", &BuildStatus::REQUEUEABLE);
    format!(
        "UPDATE exec_job r SET status = {failed}, finished_at = (now() AT TIME ZONE 'UTC'), \
         message = CASE WHEN {program_failed} THEN 'the program build failed' \
                        ELSE 'the image build failed' END \
         WHERE r.status = {created} AND ({program_failed} OR {image_failed}) \
         RETURNING r.*",
        failed = exec(ExecJobStatus::Failed),
        created = exec(ExecJobStatus::Created),
    )
});

fn refuse_sql(message: &str, from: &str, condition: &str) -> String {
    format!(
        "UPDATE exec_job r SET status = {failed}, finished_at = (now() AT TIME ZONE 'UTC'), \
         waiting_reason = NULL, message = {message} {from} \
         WHERE r.status IN ({waiting}) {condition} RETURNING r.*",
        failed = exec(ExecJobStatus::Failed),
        waiting = exec_in(&[ExecJobStatus::Created, ExecJobStatus::Queued]),
    )
}

static REFUSE_ALL_SQL: LazyLock<String> =
    LazyLock::new(|| refuse_sql(&format!("'{DISABLED_ON_INSTANCE}'"), "", ""));

static REFUSE_DISABLED_TASKS_SQL: LazyLock<String> = LazyLock::new(|| {
    refuse_sql(
        &format!(
            "CASE WHEN {build_requests} THEN '{}' ELSE '{DISABLED_FOR_TASK}' END",
            DISABLED_FOR_BUILD_REQUESTS.replace('\'', "''"),
            build_requests = build_request_task_sql("t"),
        ),
        "FROM evaluation ev JOIN task t ON t.id = ev.task JOIN project p ON p.id = t.project",
        &format!(
            "AND ev.id = r.evaluation AND NOT ({})",
            task_exec_switch_sql("t", "p")
        ),
    )
});

static FAIL_UNDISPATCHED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {failed}, message = $2, waiting_reason = NULL, \
         finished_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = $1 AND status IN ({undispatched}) RETURNING *",
        failed = exec(ExecJobStatus::Failed),
        undispatched = exec_in(&[ExecJobStatus::Queued, ExecJobStatus::Starting]),
    )
});

crate::sql_lazy! {
    FAIL_UNDISPATCHED_EXEC_JOB = || FAIL_UNDISPATCHED_SQL.as_str(),
        params = [NewUuid, Text("secret not found")],
        status_write = StatusWrite::exec_job(ExecJobStatus::Failed);
    QUEUE_CACHED_EXEC_JOBS = || QUEUE_SQL.as_str(),
        params = [],
        status_write = StatusWrite::exec_job(ExecJobStatus::Queued);
    FAIL_UNBUILT_EXEC_JOBS = || FAIL_SQL.as_str(),
        params = [],
        status_write = StatusWrite::exec_job(ExecJobStatus::Failed);
    REFUSE_WAITING_EXEC_JOBS = || REFUSE_ALL_SQL.as_str(),
        params = [],
        status_write = StatusWrite::exec_job(ExecJobStatus::Failed);
    REFUSE_EXEC_JOBS_OF_DISABLED_TASKS = || REFUSE_DISABLED_TASKS_SQL.as_str(),
        params = [],
        status_write = StatusWrite::exec_job(ExecJobStatus::Failed);
}

pub async fn refused<C: ConnectionTrait>(
    db: &C,
    instance_enabled: bool,
) -> Result<Vec<MExecJob>, DbErr> {
    let statement = if instance_enabled {
        &REFUSE_EXEC_JOBS_OF_DISABLED_TASKS
    } else {
        &REFUSE_WAITING_EXEC_JOBS
    };
    MExecJob::find_by_statement(statement.stmt()).all(db).await
}

pub async fn waiting<C: ConnectionTrait>(db: &C) -> Result<Settled, DbErr> {
    let queued = MExecJob::find_by_statement(QUEUE_CACHED_EXEC_JOBS.stmt())
        .all(db)
        .await?;
    let failed = MExecJob::find_by_statement(FAIL_UNBUILT_EXEC_JOBS.stmt())
        .all(db)
        .await?;
    Ok(Settled { queued, failed })
}

pub async fn fail_open<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    message: &str,
) -> Result<Option<MExecJob>, DbErr> {
    MExecJob::find_by_statement(FAIL_UNDISPATCHED_EXEC_JOB.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::String(Some(message.to_owned())),
    ]))
    .one(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;
    use gradient_types::{EExecJob, EvaluationId};

    #[tokio::test]
    async fn waiting_queues_created_exec_jobs_with_completed_program_build_fails_created_exec_jobs_with_failed_program_build_and_leaves_created_exec_jobs_with_building_program_build()
     {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-done.drv")
            .build_status(BuildStatus::Completed)
            .entry_point("apps.x86_64-linux.done")
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-broken.drv")
            .build_status(BuildStatus::FailedPermanent)
            .entry_point("apps.x86_64-linux.broken")
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pending.drv")
            .build_status(BuildStatus::Building)
            .entry_point("apps.x86_64-linux.pending")
            .into_database()
            .await;
        let conn = db.connection();
        let mut job_of = std::collections::HashMap::new();
        for name in ["done", "broken", "pending"] {
            let point = db
                .entry_point(evaluation, &format!("apps.x86_64-linux.{name}"))
                .await
                .expect("the entry point");
            let job = crate::rows::insert::<EExecJob, _>(
                &conn,
                MExecJob {
                    id: ExecJobId::now_v7(),
                    evaluation,
                    entry_point: point.id,
                    program_derivation: point.derivation,
                    requirement: serde_json::json!({}),
                    spec: serde_json::json!({}),
                    created_at: gradient_types::now(),
                    ..Default::default()
                },
            )
            .await
            .expect("the exec job is written");
            job_of.insert(name, job.id);
        }

        let settled = waiting(&conn).await.expect("settle the waiting exec jobs");
        let statuses: Vec<ExecJobStatus> = {
            let mut rows = db.rows::<EExecJob>().await;
            rows.sort_by_key(|row| row.id == job_of["pending"]);
            rows.iter().map(|row| row.status).collect()
        };
        db.finish().await;

        assert_eq!(
            (
                settled.queued.iter().map(|job| job.id).collect::<Vec<_>>(),
                settled.failed.iter().map(|job| job.id).collect::<Vec<_>>(),
                settled.failed[0].message.clone(),
                statuses.last().copied(),
            ),
            (
                vec![job_of["done"]],
                vec![job_of["broken"]],
                Some("the program build failed".to_owned()),
                Some(ExecJobStatus::Created),
            )
        );
    }
}
