/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_types::MRunnerJob;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use std::sync::LazyLock;

use crate::sql::status::{build_in, runner, runner_in};
use gradient_types::runner::{DISABLED_FOR_PROJECT, DISABLED_ON_INSTANCE};

#[derive(Debug, Default)]
pub struct Settled {
    pub queued: Vec<MRunnerJob>,
    pub failed: Vec<MRunnerJob>,
}

fn build_in_state(derivation: &str, statuses: &[BuildStatus]) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation_build b WHERE b.derivation = {derivation} \
         AND b.status IN ({}))",
        build_in(statuses),
    )
}

static QUEUE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE runner_job r SET status = {queued}, waiting_reason = NULL \
         WHERE r.status = {created} AND {program_done} \
         AND (r.image_derivation IS NULL OR {image_done}) \
         RETURNING r.*",
        queued = runner(RunnerJobStatus::Queued),
        created = runner(RunnerJobStatus::Created),
        program_done = build_in_state("r.program_derivation", &BuildStatus::TERMINAL_SUCCESS),
        image_done = build_in_state("r.image_derivation", &BuildStatus::TERMINAL_SUCCESS),
    )
});

static FAIL_SQL: LazyLock<String> = LazyLock::new(|| {
    let program_failed = build_in_state("r.program_derivation", &BuildStatus::REQUEUEABLE);
    let image_failed = build_in_state("r.image_derivation", &BuildStatus::REQUEUEABLE);
    format!(
        "UPDATE runner_job r SET status = {failed}, finished_at = (now() AT TIME ZONE 'UTC'), \
         message = CASE WHEN {program_failed} THEN 'the program build failed' \
                        ELSE 'the image build failed' END \
         WHERE r.status = {created} AND ({program_failed} OR {image_failed}) \
         RETURNING r.*",
        failed = runner(RunnerJobStatus::Failed),
        created = runner(RunnerJobStatus::Created),
    )
});

fn refuse_sql(message: &str, from: &str, condition: &str) -> String {
    format!(
        "UPDATE runner_job r SET status = {failed}, finished_at = (now() AT TIME ZONE 'UTC'), \
         waiting_reason = NULL, message = '{message}' {from} \
         WHERE r.status IN ({waiting}) {condition} RETURNING r.*",
        failed = runner(RunnerJobStatus::Failed),
        waiting = runner_in(&[RunnerJobStatus::Created, RunnerJobStatus::Queued]),
    )
}

static REFUSE_ALL_SQL: LazyLock<String> =
    LazyLock::new(|| refuse_sql(DISABLED_ON_INSTANCE, "", ""));

static REFUSE_DISABLED_PROJECTS_SQL: LazyLock<String> = LazyLock::new(|| {
    refuse_sql(
        DISABLED_FOR_PROJECT,
        "FROM evaluation ev JOIN task t ON t.id = ev.task JOIN project p ON p.id = t.project",
        "AND ev.id = r.evaluation AND NOT p.runner_enabled",
    )
});

crate::sql_lazy! {
    QUEUE_CACHED_RUNNER_JOBS = || QUEUE_SQL.as_str(),
        params = [];
    FAIL_UNBUILT_RUNNER_JOBS = || FAIL_SQL.as_str(),
        params = [];
    REFUSE_WAITING_RUNNER_JOBS = || REFUSE_ALL_SQL.as_str(),
        params = [];
    REFUSE_RUNNER_JOBS_OF_DISABLED_PROJECTS = || REFUSE_DISABLED_PROJECTS_SQL.as_str(),
        params = [];
}

pub async fn refused<C: ConnectionTrait>(
    db: &C,
    instance_enabled: bool,
) -> Result<Vec<MRunnerJob>, DbErr> {
    let statement = if instance_enabled {
        &REFUSE_RUNNER_JOBS_OF_DISABLED_PROJECTS
    } else {
        &REFUSE_WAITING_RUNNER_JOBS
    };
    MRunnerJob::find_by_statement(statement.stmt())
        .all(db)
        .await
}

pub async fn waiting<C: ConnectionTrait>(db: &C) -> Result<Settled, DbErr> {
    let queued = MRunnerJob::find_by_statement(QUEUE_CACHED_RUNNER_JOBS.stmt())
        .all(db)
        .await?;
    let failed = MRunnerJob::find_by_statement(FAIL_UNBUILT_RUNNER_JOBS.stmt())
        .all(db)
        .await?;
    Ok(Settled { queued, failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};

    #[tokio::test]
    async fn a_created_job_queues_once_its_builds_are_cached() {
        let db = mock_db()
            .append_query_results([Vec::<MRunnerJob>::new()])
            .append_query_results([Vec::<MRunnerJob>::new()])
            .into_connection();

        waiting(&db).await.unwrap();

        let log = all_statements(db.into_transaction_log());
        let queue = &log[0].sql;
        assert!(queue.starts_with("UPDATE runner_job"), "{queue}");
        assert!(queue.contains("b.status IN (3, 7)"), "{queue}");
        assert!(queue.contains("RETURNING"), "{queue}");
        let fail = &log[1].sql;
        assert!(fail.contains("b.status IN (4, 5, 6, 9)"), "{fail}");
        assert!(fail.contains("'the program build failed'"), "{fail}");
    }
}
