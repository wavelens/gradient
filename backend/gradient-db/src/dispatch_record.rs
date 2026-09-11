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
use gradient_entity::ids::DispatchedJobId;
use sea_orm::sea_query::{Expr, Value};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, QueryFilter};

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

/// Close every open row of `worker_id` dispatched before `before` as
/// `Abandoned`; returns how many closed. The cutoff keeps a reconnecting
/// worker off the rows its own still-landing terminal reports close: only a
/// process that is gone leaves a row no live closer owns.
pub async fn abandon_open_dispatches_for_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    before: chrono::NaiveDateTime,
) -> Result<u64, DbErr> {
    abandon_open(
        db,
        Some(
            CDispatchedJob::WorkerId
                .eq(worker_id)
                .and(CDispatchedJob::DispatchedAt.lt(before)),
        ),
    )
    .await
}

/// Close every open row as `Abandoned`; returns how many closed. Startup
/// recovery's closer: nothing a previous process handed out is still out.
pub async fn abandon_all_open_dispatches<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    abandon_open(db, None).await
}

/// Close the open rows of the given job keys as `Abandoned`; returns how many closed.
pub async fn abandon_open_dispatches_for_jobs<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<u64, DbErr> {
    if job_keys.is_empty() {
        return Ok(0);
    }

    abandon_open(db, Some(CDispatchedJob::JobId.is_in(job_keys.to_vec()))).await
}

/// Close the open row of one dispatch as `Abandoned`; returns how many closed.
pub async fn abandon_open_dispatch<C: ConnectionTrait>(
    db: &C,
    dispatch: DispatchedJobId,
) -> Result<u64, DbErr> {
    abandon_open(db, Some(CDispatchedJob::Id.eq(dispatch))).await
}

/// Close the open rows of the named dispatches as `Abandoned`; returns how
/// many closed. The scope is one array bind, not one placeholder per id: the
/// sweep reaps a whole backlog in one statement and `IN (..)` would blow
/// Postgres' 65535-bind-parameter cap long before it got there.
pub async fn abandon_open_dispatches<C: ConnectionTrait>(
    db: &C,
    dispatches: &[DispatchedJobId],
) -> Result<u64, DbErr> {
    if dispatches.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = dispatches.iter().map(|d| d.into_inner()).collect();

    abandon_open(
        db,
        Some(Expr::cust_with_values(
            r#""dispatched_job"."id" = ANY($1)"#,
            [Value::from(ids)],
        )),
    )
    .await
}

async fn abandon_open<C: ConnectionTrait>(db: &C, scope: Option<Expr>) -> Result<u64, DbErr> {
    let mut update = EDispatchedJob::update_many()
        .col_expr(
            CDispatchedJob::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(
            CDispatchedJob::Outcome,
            Expr::value(i16::from(DispatchedJobOutcome::Abandoned)),
        );
    if let Some(scope) = scope {
        update = update.filter(scope);
    }

    let res = update
        .filter(CDispatchedJob::FinishedAt.is_null())
        .exec(db)
        .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement};

    fn closed_rows(rows_affected: u64) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected,
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

    /// The cutoff is the whole point of the worker-scoped closer: a row this
    /// process dispatched has a live closer in the report that is landing for
    /// it, and rewriting it as `Abandoned` would lose that outcome. Only rows
    /// an earlier process handed out are the registration's to close.
    #[tokio::test]
    async fn a_workers_open_rows_close_as_abandoned_only_below_the_cutoff() {
        let db = closed_rows(2).into_connection();
        let before = gradient_types::now();

        let closed = abandon_open_dispatches_for_worker(&db, "w1", before)
            .await
            .expect("update");

        assert_eq!(closed, 2);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(sql.contains("\"worker_id\" ="), "{sql}");
        assert!(sql.contains("\"dispatched_at\" <"), "{sql}");

        let values = format!("{:?}", statement.values);
        assert!(values.contains("String(Some(\"w1\"))"), "{values}");
        assert!(values.contains(&format!("{before:?}")), "{values}");
    }

    /// Startup's closer is unscoped by design: the tracker that knew which
    /// jobs were out died with the process, so every open row is stale.
    #[tokio::test]
    async fn every_open_row_closes_for_startup_recovery() {
        let db = closed_rows(7).into_connection();

        let closed = abandon_all_open_dispatches(&db).await.expect("update");

        assert_eq!(closed, 7);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(!sql.contains("\"worker_id\""), "{sql}");
        assert!(!sql.contains("\"job_id\""), "{sql}");
        assert!(!sql.contains("\"dispatched_at\""), "{sql}");
    }

    /// The sweep closes its whole backlog in one statement, so the scope has to
    /// be a single array bind: one placeholder per id caps the reaper at
    /// Postgres' 65535 binds and the statement fails outright above that.
    #[tokio::test]
    async fn the_named_dispatches_close_under_one_array_bind() {
        let db = closed_rows(2).into_connection();
        let reap = [DispatchedJobId::now_v7(), DispatchedJobId::now_v7()];

        let closed = abandon_open_dispatches(&db, &reap).await.expect("update");

        assert_eq!(closed, 2);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(sql.contains("\"dispatched_job\".\"id\" = ANY($3)"), "{sql}");
        assert!(!sql.contains(" IN ("), "{sql}");
        assert!(!sql.contains("$4"), "{sql}");

        let values = format!("{:?}", statement.values);
        assert!(
            values.contains(&format!(
                "Array(Uuid, Some([Uuid(Some({})), Uuid(Some({}))]))",
                reap[0], reap[1]
            )),
            "{values}"
        );
    }

    #[tokio::test]
    async fn no_dispatches_means_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let closed = abandon_open_dispatches(&db, &[]).await.expect("no-op");

        assert_eq!(closed, 0);
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn the_named_jobs_open_rows_close_as_abandoned() {
        let db = closed_rows(2).into_connection();
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
    async fn one_dispatchs_open_row_closes_as_abandoned() {
        let db = closed_rows(1).into_connection();
        let dispatch = DispatchedJobId::now_v7();

        let closed = abandon_open_dispatch(&db, dispatch).await.expect("update");

        assert_eq!(closed, 1);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(sql.contains("\"dispatched_job\".\"id\" = $3"), "{sql}");
        assert!(!sql.contains("\"job_id\""), "{sql}");
        assert!(!sql.contains("\"worker_id\""), "{sql}");

        let values = format!("{:?}", statement.values);
        assert!(
            values.contains(&format!("Uuid(Some({dispatch}))")),
            "{values}"
        );
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
