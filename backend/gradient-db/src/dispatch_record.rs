/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `dispatched_job` row is the one proof that a job is out. The scheduler's
//! job key (`eval:<evaluation>` / `build:<anchor>`) is persisted on `job_id`
//! and rebuilt in SQL by the dispatch gates, so the prefixes live here next to
//! the SQL and a copy that drifts cannot silently open the gate.

use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobOutcome, Entity as EDispatchedJob,
};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub const EVAL_KEY_PREFIX: &str = "eval:";
pub const BUILD_KEY_PREFIX: &str = "build:";

/// The evaluation job key of the uuid column `id_expr`, as SQL text.
pub fn eval_job_key_sql(id_expr: &str) -> String {
    format!("'{EVAL_KEY_PREFIX}' || {id_expr}::text")
}

/// The build job key of the anchor uuid column `id_expr`, as SQL text.
pub fn build_job_key_sql(id_expr: &str) -> String {
    format!("'{BUILD_KEY_PREFIX}' || {id_expr}::text")
}

/// SQL predicate: no `dispatched_job` row for `job_key_sql` is still open.
/// Served by `idx-dispatched_job-open-by-job-id`.
pub fn no_open_dispatch_predicate(job_key_sql: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = {job_key_sql} \
         AND dj.finished_at IS NULL)"
    )
}

/// Close every open row of `worker_id` as `Abandoned`; returns how many closed.
pub async fn abandon_open_dispatches_for_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<u64, DbErr> {
    abandon_open(db, CDispatchedJob::WorkerId.eq(worker_id)).await
}

/// Close the open rows of the given job keys as `Abandoned`; returns how many closed.
pub async fn abandon_open_dispatches_for_jobs<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<u64, DbErr> {
    if job_keys.is_empty() {
        return Ok(0);
    }

    abandon_open(db, CDispatchedJob::JobId.is_in(job_keys.to_vec())).await
}

async fn abandon_open<C: ConnectionTrait>(db: &C, scope: Expr) -> Result<u64, DbErr> {
    let res = EDispatchedJob::update_many()
        .col_expr(
            CDispatchedJob::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(
            CDispatchedJob::Outcome,
            Expr::value(i16::from(DispatchedJobOutcome::Abandoned)),
        )
        .filter(scope)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .exec(db)
        .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement};

    fn closed_one_row() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 2,
        }])
    }

    /// The statement's `sql` carries placeholders only, so nothing but the bound
    /// value pins the outcome the closers write. Built from the enum so a moved
    /// discriminant stays in step, and rendered the way sea-query's `Value`
    /// derives `Debug` for a bound `i16`.
    fn abandoned_bound() -> String {
        format!(
            "SmallInt(Some({}))",
            i16::from(DispatchedJobOutcome::Abandoned)
        )
    }

    fn assert_closes_open_rows_as_abandoned(statement: &Statement) {
        let sql = &statement.sql;
        assert!(
            sql.starts_with("UPDATE \"dispatched_job\" SET \"finished_at\" = $1, \"outcome\" = $2"),
            "{sql}"
        );
        assert!(sql.contains("\"finished_at\" IS NULL"), "{sql}");

        let values = format!("{:?}", statement.values);
        assert!(values.contains(&abandoned_bound()), "{values}");
    }

    #[test]
    fn the_predicate_rebuilds_the_trackers_key() {
        assert_eq!(eval_job_key_sql("ev.id"), "'eval:' || ev.id::text");
        assert_eq!(build_job_key_sql("db.id"), "'build:' || db.id::text");
        assert_eq!(
            no_open_dispatch_predicate(&eval_job_key_sql("ev.id")),
            "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = 'eval:' || ev.id::text \
             AND dj.finished_at IS NULL)"
        );
    }

    #[tokio::test]
    async fn a_workers_open_rows_close_as_abandoned() {
        let db = closed_one_row().into_connection();

        let closed = abandon_open_dispatches_for_worker(&db, "w1")
            .await
            .expect("update");

        assert_eq!(closed, 2);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(sql.contains("\"worker_id\" ="), "{sql}");

        let values = format!("{:?}", statement.values);
        assert!(values.contains("String(Some(\"w1\"))"), "{values}");
    }

    #[tokio::test]
    async fn the_named_jobs_open_rows_close_as_abandoned() {
        let db = closed_one_row().into_connection();
        let keys = vec![
            format!("{BUILD_KEY_PREFIX}019905f2-0000-7000-8000-000000000001"),
            format!("{EVAL_KEY_PREFIX}019905f2-0000-7000-8000-000000000002"),
        ];

        let closed = abandon_open_dispatches_for_jobs(&db, &keys)
            .await
            .expect("update");

        assert_eq!(closed, 2);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(sql.contains("\"job_id\" IN ("), "{sql}");

        let values = format!("{:?}", statement.values);
        for key in &keys {
            assert!(
                values.contains(&format!("String(Some(\"{key}\"))")),
                "{values}"
            );
        }
    }

    #[tokio::test]
    async fn no_job_keys_means_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let closed = abandon_open_dispatches_for_jobs(&db, &[])
            .await
            .expect("no-op");

        assert_eq!(closed, 0);
        assert!(db.into_transaction_log().is_empty());
    }
}
