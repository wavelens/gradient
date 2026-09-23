/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `dispatched_job` row is the one proof that a job is out. The scheduler's
//! job key (`eval:<evaluation>` / `build:<anchor>`) is persisted on `job_id`
//! and rebuilt in SQL by the dispatch gates, so the prefixes live here next to
//! the SQL and a copy that drifts cannot silently open the gate.

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobKind, DispatchedJobOutcome, Entity as EDispatchedJob,
    Model as MDispatchedJob,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DerivationBuildId, DispatchedJobId, EvaluationId};
use gradient_types::{CDerivationBuild, CEvaluation};
use sea_orm::sea_query::{Expr, InsertStatement, OnConflict, Query, Value};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, IntoActiveModel,
    Iterable, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

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
/// Served by `idx-dispatched_job-open-job`.
pub fn no_open_dispatch_predicate(job_key_sql: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = {job_key_sql} \
         AND dj.finished_at IS NULL)"
    )
}

/// What a claim re-reads in the statement that writes its row: a job assembled
/// from a snapshot goes out only while its subject still wants it.
#[derive(Debug, Clone, Copy)]
pub enum ClaimGate {
    /// The anchor is still `Queued` in the relay mode the job was assembled for.
    Build {
        anchor: DerivationBuildId,
        substitute: bool,
    },
    /// The evaluation has not finished.
    Eval { evaluation: EvaluationId },
}

impl ClaimGate {
    fn holds(self) -> Expr {
        let subject = match self {
            ClaimGate::Build { anchor, substitute } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::derivation_build::Entity)
                .and_where(CDerivationBuild::Id.eq(anchor))
                .and_where(CDerivationBuild::Status.eq(BuildStatus::Queued))
                .and_where(CDerivationBuild::Substitutable.eq(substitute))
                .to_owned(),
            ClaimGate::Eval { evaluation } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::evaluation::Entity)
                .and_where(CEvaluation::Id.eq(evaluation))
                .and_where(CEvaluation::Status.is_not_in([
                    EvaluationStatus::Completed,
                    EvaluationStatus::Failed,
                    EvaluationStatus::Aborted,
                ]))
                .to_owned(),
        };

        Expr::exists(subject)
    }
}

/// Claim a job by writing its open `dispatched_job` row; `false` when the claim
/// was lost. One statement, arbitrated by the unique open-row index
/// `idx-dispatched_job-open-job`: of any number of instances claiming one job
/// key, exactly one inserts, and a gate that no longer holds inserts nothing.
pub async fn claim_dispatch<C: ConnectionTrait>(
    db: &C,
    row: MDispatchedJob,
    gate: ClaimGate,
) -> Result<bool, DbErr> {
    let claimed = db.execute(&claim_statement(row, gate)?).await?;

    Ok(claimed.rows_affected() == 1)
}

fn claim_statement(row: MDispatchedJob, gate: ClaimGate) -> Result<InsertStatement, DbErr> {
    let row = row.into_active_model();
    let (columns, values): (Vec<CDispatchedJob>, Vec<Expr>) = CDispatchedJob::iter()
        .filter_map(|c| row.get(c).into_value().map(|v| (c, Expr::val(v))))
        .unzip();

    let gated = Query::select()
        .exprs(values)
        .and_where(gate.holds())
        .to_owned();
    let mut insert = Query::insert();
    insert
        .into_table(EDispatchedJob)
        .columns(columns)
        .select_from(gated)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .on_conflict(
            OnConflict::column(CDispatchedJob::JobId)
                .target_and_where(Expr::col(CDispatchedJob::FinishedAt).is_null())
                .do_nothing()
                .to_owned(),
        );

    Ok(insert)
}

/// The newest eval job of each evaluation, its Job Board entry. Served by
/// `idx-dispatched_job-eval-by-evaluation`.
pub async fn latest_eval_jobs<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, DispatchedJobId>, DbErr> {
    let rows = crate::fetch_in_chunks(evaluations, |chunk| async move {
        EDispatchedJob::find()
            .select_only()
            .column(CDispatchedJob::EvaluationId)
            .column(CDispatchedJob::Id)
            .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
            .filter(CDispatchedJob::EvaluationId.is_in(chunk))
            .order_by_asc(CDispatchedJob::DispatchedAt)
            .into_tuple::<(EvaluationId, DispatchedJobId)>()
            .all(db)
            .await
    })
    .await?;

    Ok(rows.into_iter().collect())
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

/// How many rows one startup-recovery statement closes.
const RECOVERY_CLOSE_CHUNK: u64 = 10_000;

/// Close every open row as `Abandoned`; returns how many closed. Startup
/// recovery's closer: nothing a previous process handed out is still out.
///
/// The predicate stays unbounded and the statement is chunked instead. A
/// backlog runs to six figures, every close is a non-HOT update because
/// `finished_at` is indexed, and this runs before the listener binds: one
/// statement over the whole set is a single transaction holding hundreds of
/// megabytes of WAL and row locks with nothing able to interleave, where the
/// same total work split at [`RECOVERY_CLOSE_CHUNK`] rows lets autovacuum keep
/// up and bounds every lock to one chunk.
pub async fn abandon_all_open_dispatches<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let chunk = format!(
        "\"dispatched_job\".\"id\" IN (SELECT id FROM dispatched_job \
         WHERE finished_at IS NULL LIMIT {RECOVERY_CLOSE_CHUNK})"
    );

    let mut closed = 0;
    loop {
        let rows = abandon_open(db, Some(Expr::cust(chunk.clone()))).await?;
        closed += rows;
        if rows < RECOVERY_CLOSE_CHUNK {
            return Ok(closed);
        }
    }
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

    fn claim_row(job_id: &str) -> MDispatchedJob {
        MDispatchedJob {
            id: DispatchedJobId::now_v7(),
            job_id: Some(job_id.to_owned()),
            worker_id: "w1".into(),
            ..Default::default()
        }
    }

    async fn claimed(rows_affected: u64, gate: ClaimGate) -> (bool, Statement) {
        let db = closed_rows(rows_affected).into_connection();
        let won = claim_dispatch(&db, claim_row("build:x"), gate)
            .await
            .expect("claim");
        let log = db.into_transaction_log();

        (won, log[0].statements()[0].clone())
    }

    fn build_gate() -> ClaimGate {
        ClaimGate::Build {
            anchor: DerivationBuildId::now_v7(),
            substitute: false,
        }
    }

    /// The unique open-row index is the arbiter: the insert names it through
    /// its `ON CONFLICT` target, so a rival's open row turns the claim into a
    /// no-op instead of a second hand-out.
    #[tokio::test]
    async fn a_claim_inserts_its_row_unless_the_job_is_already_open() {
        let (won, statement) = claimed(1, build_gate()).await;

        assert!(won);
        let sql = &statement.sql;
        assert!(sql.starts_with("INSERT INTO \"dispatched_job\""), "{sql}");
        assert!(
            sql.contains("ON CONFLICT (\"job_id\") WHERE \"finished_at\" IS NULL DO NOTHING"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn a_claim_that_inserted_nothing_is_lost() {
        let (won, _) = claimed(0, build_gate()).await;

        assert!(!won);
    }

    /// The anchor is re-read in the claim itself: a job assembled while it was
    /// `Queued` as a build must not go out once it moved or became a relay.
    #[tokio::test]
    async fn a_build_claim_requires_the_anchor_queued_in_its_relay_mode() {
        let (_, statement) = claimed(1, build_gate()).await;

        let sql = &statement.sql;
        assert!(sql.contains("WHERE EXISTS(SELECT"), "{sql}");
        assert!(sql.contains("FROM \"derivation_build\""), "{sql}");
        assert!(sql.contains("\"derivation_build\".\"status\" ="), "{sql}");
        assert!(
            sql.contains("\"derivation_build\".\"substitutable\" ="),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn an_eval_claim_requires_an_unfinished_evaluation() {
        let (_, statement) = claimed(
            1,
            ClaimGate::Eval {
                evaluation: EvaluationId::now_v7(),
            },
        )
        .await;

        let sql = &statement.sql;
        assert!(sql.contains("FROM \"evaluation\""), "{sql}");
        assert!(sql.contains("\"evaluation\".\"status\" NOT IN"), "{sql}");
        assert!(!sql.contains("\"derivation_build\""), "{sql}");
    }

    #[tokio::test]
    async fn the_newest_eval_job_wins() {
        let evaluation = EvaluationId::now_v7();
        let (older, newer) = (DispatchedJobId::now_v7(), DispatchedJobId::now_v7());
        let row = |job: DispatchedJobId| {
            std::collections::BTreeMap::from([
                (
                    "evaluation_id",
                    sea_orm::Value::from(evaluation.into_inner()),
                ),
                ("id", sea_orm::Value::from(job.into_inner())),
            ])
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(older), row(newer)]])
            .into_connection();

        let jobs = latest_eval_jobs(&db, &[evaluation]).await.unwrap();

        assert_eq!(jobs.get(&evaluation), Some(&newer));
        let log = db.into_transaction_log();
        let sql = &log[0].statements()[0].sql;
        assert!(sql.contains("\"kind\" = $1"), "{sql}");
        assert!(
            sql.ends_with("ORDER BY \"dispatched_job\".\"dispatched_at\" ASC"),
            "{sql}"
        );
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

    /// Startup's predicate is unscoped by design: the tracker that knew which
    /// jobs were out died with the process, so every open row is stale. Only
    /// the statement is bounded, and a chunk narrower than the backlog is what
    /// keeps the whole close out of one pre-listener transaction.
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
        assert!(sql.contains(&chunk_scope()), "{sql}");
    }

    fn chunk_scope() -> String {
        format!(
            "IN (SELECT id FROM dispatched_job WHERE finished_at IS NULL \
             LIMIT {RECOVERY_CLOSE_CHUNK})"
        )
    }

    /// The loop is the whole of the chunking: a statement that fills its chunk
    /// means rows may remain, a short one is the end. Exactly two exec results
    /// are supplied, so a third statement would draw an empty buffer and turn
    /// the call into an `Err`.
    #[tokio::test]
    async fn startup_recovery_closes_a_backlog_one_chunk_per_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: RECOVERY_CLOSE_CHUNK,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 7,
                },
            ])
            .into_connection();

        let closed = abandon_all_open_dispatches(&db).await.expect("update");

        assert_eq!(closed, RECOVERY_CLOSE_CHUNK + 7);
        let log = db.into_transaction_log();
        assert_eq!(log.len(), 2);
        for transaction in &log {
            let statement = &transaction.statements()[0];
            assert_closes_open_rows_as_abandoned(statement);
            assert!(statement.sql.contains(&chunk_scope()), "{}", statement.sql);
        }
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
