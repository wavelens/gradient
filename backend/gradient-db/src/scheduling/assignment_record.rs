/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The job key prefixes are living next to the SQL rebuilding them.
//! A drifting copy could silently open a dispatch gate.

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobKind, DispatchedJobOutcome, Entity as EDispatchedJob,
    Model as MDispatchedJob,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DerivationBuildId, DispatchedJobId, EvaluationId, RunnerJobId};
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_types::{CDerivationBuild, CEvaluation, CRunnerJob};
use sea_orm::sea_query::{Expr, InsertStatement, OnConflict, Query, Value};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, IntoActiveModel,
    Iterable, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

pub const EVAL_KEY_PREFIX: &str = "eval:";
pub const BUILD_KEY_PREFIX: &str = "build:";
pub const RUNNER_KEY_PREFIX: &str = "runner:";

pub fn shared_build_of_key(job_id: &str) -> Option<DerivationBuildId> {
    job_id.strip_prefix(BUILD_KEY_PREFIX)?.parse().ok()
}

pub fn eval_of_key(job_id: &str) -> Option<EvaluationId> {
    job_id.strip_prefix(EVAL_KEY_PREFIX)?.parse().ok()
}

pub fn runner_job_of_key(job_id: &str) -> Option<RunnerJobId> {
    job_id.strip_prefix(RUNNER_KEY_PREFIX)?.parse().ok()
}

pub fn runner_job_key_sql(id_expr: &str) -> String {
    format!("'{RUNNER_KEY_PREFIX}' || {id_expr}::text")
}

pub fn eval_job_key_sql(id_expr: &str) -> String {
    format!("'{EVAL_KEY_PREFIX}' || {id_expr}::text")
}

pub fn build_job_key_sql(id_expr: &str) -> String {
    format!("'{BUILD_KEY_PREFIX}' || {id_expr}::text")
}

pub fn no_open_assignment_predicate(job_key_sql: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = {job_key_sql} \
         AND dj.finished_at IS NULL)"
    )
}

#[derive(Debug, Clone, Copy)]
pub enum ClaimGate {
    Build {
        shared_build: DerivationBuildId,
        substitute: bool,
    },
    Eval {
        evaluation: EvaluationId,
    },
    Runner {
        runner_job: RunnerJobId,
    },
}

impl ClaimGate {
    fn holds(self) -> Expr {
        let subject = match self {
            ClaimGate::Build {
                shared_build,
                substitute,
            } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::derivation_build::Entity)
                .and_where(CDerivationBuild::Id.eq(shared_build))
                .and_where(CDerivationBuild::Status.eq(BuildStatus::Queued))
                .and_where(CDerivationBuild::CacheAvailable.eq(substitute))
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
            ClaimGate::Runner { runner_job } => Query::select()
                .expr(Expr::val(1))
                .from(gradient_entity::runner_job::Entity)
                .and_where(CRunnerJob::Id.eq(runner_job))
                .and_where(CRunnerJob::Status.eq(RunnerJobStatus::Queued))
                .to_owned(),
        };

        Expr::exists(subject)
    }
}

pub async fn claim_assignment<C: ConnectionTrait>(
    db: &C,
    row: MDispatchedJob,
    gate: ClaimGate,
) -> Result<bool, DbErr> {
    let claimed = db.execute(&claim_statement(row, gate)?).await?;

    Ok(claimed.rows_affected() == 1)
}

pub(crate) fn claim_statement(
    row: MDispatchedJob,
    gate: ClaimGate,
) -> Result<InsertStatement, DbErr> {
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

pub async fn eval_attempts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<u64, DbErr> {
    EDispatchedJob::find()
        .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
        .filter(CDispatchedJob::EvaluationId.eq(evaluation))
        .filter(
            CDispatchedJob::Outcome
                .is_null()
                .or(CDispatchedJob::Outcome.ne(DispatchedJobOutcome::Abandoned)),
        )
        .count(db)
        .await
}

pub async fn abandon_open_assignments_for_worker<C: ConnectionTrait>(
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

const RECOVERY_CLOSE_CHUNK: u64 = 10_000;

/// The statement is chunked at [`RECOVERY_CLOSE_CHUNK`] rows.
/// One statement over a six-figure backlog would hold WAL and row locks before the
/// listener is bound.
pub async fn abandon_open_cluster_assignments<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let chunk = format!(
        "\"dispatched_job\".\"id\" IN (SELECT id FROM dispatched_job \
         WHERE finished_at IS NULL AND (cluster_attempt IS NOT NULL OR job_id IS NULL) \
         LIMIT {RECOVERY_CLOSE_CHUNK})"
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

pub async fn abandon_open_assignments_for_jobs<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<u64, DbErr> {
    if job_keys.is_empty() {
        return Ok(0);
    }

    abandon_open(db, Some(CDispatchedJob::JobId.is_in(job_keys.to_vec()))).await
}

pub async fn abandon_open_assignment<C: ConnectionTrait>(
    db: &C,
    assignment_id: DispatchedJobId,
) -> Result<u64, DbErr> {
    abandon_open(db, Some(CDispatchedJob::Id.eq(assignment_id))).await
}

pub async fn abandon_open_assignments<C: ConnectionTrait>(
    db: &C,
    assignments: &[DispatchedJobId],
) -> Result<u64, DbErr> {
    if assignments.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = assignments.iter().map(|d| d.into_inner()).collect();

    abandon_open(
        db,
        Some(Expr::cust_with_values(
            r#""dispatched_job"."id" = ANY($1)"#,
            [Value::from(ids)],
        )),
    )
    .await
}

pub async fn open_assignment_keys<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(DispatchedJobId, String, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .column(CDispatchedJob::WorkerId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .into_tuple()
        .all(db)
        .await
}

pub async fn open_single_assignments_of_disconnected_workers<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(DispatchedJobId, String, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .column(CDispatchedJob::WorkerId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::ClusterAttempt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .filter(Expr::cust(
            "NOT EXISTS (SELECT 1 FROM worker_connection \
             WHERE worker_connection.worker_id = \"dispatched_job\".\"worker_id\" \
             AND worker_connection.disconnected_at IS NULL)",
        ))
        .into_tuple()
        .all(db)
        .await
}

pub async fn keys_with_open_assignment<C: ConnectionTrait>(
    db: &C,
    job_keys: &[String],
) -> Result<Vec<String>, DbErr> {
    if job_keys.is_empty() {
        return Ok(Vec::new());
    }

    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::JobId)
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::JobId.is_in(job_keys.to_vec()))
        .distinct()
        .into_tuple()
        .all(db)
        .await
}

pub async fn open_single_assignments_of_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Vec<(DispatchedJobId, String)>, DbErr> {
    EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .filter(CDispatchedJob::WorkerId.eq(worker_id))
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::ClusterAttempt.is_null())
        .filter(CDispatchedJob::JobId.is_not_null())
        .into_tuple()
        .all(db)
        .await
}

pub async fn assignment_rows<C: ConnectionTrait>(
    db: &C,
    ids: &[DispatchedJobId],
) -> Result<Vec<MDispatchedJob>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        EDispatchedJob::find()
            .filter(CDispatchedJob::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn build_has_open_assignment<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
) -> Result<bool, DbErr> {
    let open = EDispatchedJob::find()
        .filter(CDispatchedJob::JobId.eq(format!("{BUILD_KEY_PREFIX}{shared_build}")))
        .filter(CDispatchedJob::FinishedAt.is_null())
        .one(db)
        .await?;

    Ok(open.is_some())
}

pub(crate) async fn abandon_open<C: ConnectionTrait>(
    db: &C,
    scope: Option<Expr>,
) -> Result<u64, DbErr> {
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
    use gradient_fixtures::mock::{empty_db, mock_db};
    use sea_orm::{MockDatabase, MockExecResult, Statement};

    #[test]
    fn a_runner_key_names_its_job() {
        let id = gradient_entity::ids::RunnerJobId::now_v7();
        assert_eq!(
            runner_job_of_key(&format!("{RUNNER_KEY_PREFIX}{id}")),
            Some(id)
        );
        assert_eq!(runner_job_of_key(&format!("{BUILD_KEY_PREFIX}{id}")), None);
    }

    fn closed_rows(rows_affected: u64) -> MockDatabase {
        mock_db().append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }])
    }

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
        let won = claim_assignment(&db, claim_row("build:x"), gate)
            .await
            .expect("claim");
        let log = db.into_transaction_log();

        (won, log[0].statements()[0].clone())
    }

    fn build_gate() -> ClaimGate {
        ClaimGate::Build {
            shared_build: DerivationBuildId::now_v7(),
            substitute: false,
        }
    }

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

    #[tokio::test]
    async fn a_build_claim_requires_the_shared_build_queued_in_its_passthrough_mode() {
        let (_, statement) = claimed(1, build_gate()).await;

        let sql = &statement.sql;
        assert!(sql.contains("WHERE EXISTS(SELECT"), "{sql}");
        assert!(sql.contains("FROM \"derivation_build\""), "{sql}");
        assert!(sql.contains("\"derivation_build\".\"status\" ="), "{sql}");
        assert!(
            sql.contains("\"derivation_build\".\"cache_available\" ="),
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
        let db = mock_db()
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
    fn only_a_build_key_names_a_shared_build() {
        let shared_build = DerivationBuildId::now_v7();
        assert_eq!(
            shared_build_of_key(&format!("{BUILD_KEY_PREFIX}{shared_build}")),
            Some(shared_build)
        );
        assert_eq!(
            shared_build_of_key(&format!("{EVAL_KEY_PREFIX}{}", EvaluationId::now_v7())),
            None
        );
        assert_eq!(shared_build_of_key("build:not-a-uuid"), None);
    }

    #[test]
    fn only_an_eval_key_names_an_evaluation() {
        let evaluation = EvaluationId::now_v7();
        assert_eq!(
            eval_of_key(&format!("{EVAL_KEY_PREFIX}{evaluation}")),
            Some(evaluation)
        );
        assert_eq!(
            eval_of_key(&format!(
                "{BUILD_KEY_PREFIX}{}",
                DerivationBuildId::now_v7()
            )),
            None
        );
    }

    #[test]
    fn the_predicate_rebuilds_the_trackers_key() {
        assert_eq!(build_job_key_sql("db.id"), "'build:' || db.id::text");
        assert_eq!(
            no_open_assignment_predicate(&build_job_key_sql("db.id")),
            "NOT EXISTS (SELECT 1 FROM dispatched_job dj WHERE dj.job_id = 'build:' || db.id::text \
             AND dj.finished_at IS NULL)"
        );
    }

    #[tokio::test]
    async fn a_workers_open_rows_close_as_abandoned_only_below_the_cutoff() {
        let db = closed_rows(2).into_connection();
        let before = gradient_types::now();

        let closed = abandon_open_assignments_for_worker(&db, "w1", before)
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

    #[tokio::test]
    async fn startup_recovery_closes_only_open_cluster_rows() {
        let db = closed_rows(7).into_connection();

        let closed = abandon_open_cluster_assignments(&db).await.expect("update");

        assert_eq!(closed, 7);
        let log = db.into_transaction_log();
        let statement = &log[0].statements()[0];
        assert_closes_open_rows_as_abandoned(statement);

        let sql = &statement.sql;
        assert!(!sql.contains("\"worker_id\""), "{sql}");
        assert!(!sql.contains("\"dispatched_at\""), "{sql}");
        assert!(sql.contains(&chunk_scope()), "{sql}");
    }

    fn chunk_scope() -> String {
        format!(
            "IN (SELECT id FROM dispatched_job WHERE finished_at IS NULL \
             AND (cluster_attempt IS NOT NULL OR job_id IS NULL) LIMIT {RECOVERY_CLOSE_CHUNK})"
        )
    }

    #[tokio::test]
    async fn startup_recovery_closes_a_backlog_one_chunk_per_statement() {
        let db = mock_db()
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

        let closed = abandon_open_cluster_assignments(&db).await.expect("update");

        assert_eq!(closed, RECOVERY_CLOSE_CHUNK + 7);
        let log = db.into_transaction_log();
        assert_eq!(log.len(), 2);
        for transaction in &log {
            let statement = &transaction.statements()[0];
            assert_closes_open_rows_as_abandoned(statement);
            assert!(statement.sql.contains(&chunk_scope()), "{}", statement.sql);
        }
    }

    #[tokio::test]
    async fn the_named_assignments_close_under_one_array_bind() {
        let db = closed_rows(2).into_connection();
        let reap = [DispatchedJobId::now_v7(), DispatchedJobId::now_v7()];

        let closed = abandon_open_assignments(&db, &reap).await.expect("update");

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
    async fn no_assignments_means_no_statement() {
        let db = empty_db();

        let closed = abandon_open_assignments(&db, &[]).await.expect("no-op");

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

        let closed = abandon_open_assignments_for_jobs(&db, &keys)
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
    async fn one_assignments_open_row_closes_as_abandoned() {
        let db = closed_rows(1).into_connection();
        let assignment_id = DispatchedJobId::now_v7();

        let closed = abandon_open_assignment(&db, assignment_id)
            .await
            .expect("update");

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
            values.contains(&format!("Uuid(Some({assignment_id}))")),
            "{values}"
        );
    }

    #[tokio::test]
    async fn no_job_keys_means_no_statement() {
        let db = empty_db();

        let closed = abandon_open_assignments_for_jobs(&db, &[])
            .await
            .expect("no-op");

        assert_eq!(closed, 0);
        assert!(db.into_transaction_log().is_empty());
    }
}
