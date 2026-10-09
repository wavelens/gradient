/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `evaluation_shared_build_*` triggers are the only writers of the ledger.

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EvalCounters {
    pub named: i64,
    pub active: i64,
    pub failed: i64,
    pub queued: i64,
    pub building: i64,
}

impl EvalCounters {
    fn from_row(row: &QueryResult) -> Result<Self, DbErr> {
        Ok(Self {
            named: row.try_get("", "named")?,
            active: row.try_get("", "active")?,
            failed: row.try_get("", "failed")?,
            queued: row.try_get("", "queued")?,
            building: row.try_get("", "building")?,
        })
    }
}

crate::sql! {
    EVAL_COUNTERS = "SELECT \
        (e.named_shared_builds + coalesce(sum(d.named), 0))::bigint AS named, \
        (e.active_shared_builds + coalesce(sum(d.active), 0))::bigint AS active, \
        (e.failed_shared_builds + coalesce(sum(d.failed), 0))::bigint AS failed, \
        (e.queued_shared_builds + coalesce(sum(d.queued), 0))::bigint AS queued, \
        (e.building_shared_builds + coalesce(sum(d.building), 0))::bigint AS building \
        FROM evaluation e LEFT JOIN evaluation_shared_build_delta d ON d.evaluation = e.id \
        WHERE e.id = $1 GROUP BY e.id",
        params = [EvaluationId],
        tier = Hot;

    IN_FLIGHT_COUNTERS = "SELECT e.id, \
        (e.named_shared_builds + coalesce(sum(d.named), 0))::bigint AS named, \
        (e.active_shared_builds + coalesce(sum(d.active), 0))::bigint AS active, \
        (e.failed_shared_builds + coalesce(sum(d.failed), 0))::bigint AS failed, \
        (e.queued_shared_builds + coalesce(sum(d.queued), 0))::bigint AS queued, \
        (e.building_shared_builds + coalesce(sum(d.building), 0))::bigint AS building \
        FROM evaluation e LEFT JOIN evaluation_shared_build_delta d ON d.evaluation = e.id \
        WHERE e.id = ANY($1::uuid[]) GROUP BY e.id",
        params = [EvaluationIds(64)],
        tier = Bulk;

    FOLD_LOCK_TRY = "SELECT pg_try_advisory_xact_lock($1) AS locked",
        params = [Int(640)],
        tier = Hot;

    FOLD_LOCK_WAIT = "SELECT pg_advisory_xact_lock($1)",
        params = [Int(640)],
        tier = Hot;

    // SKIP LOCKED is keeping the fold and the recount from waiting on an evaluation row.
    // Neither can close a deadlock with a writer holding one.
    // A skipped evaluation is staying in the ledger for the next pass.
    FOLD_SHARED_BUILD_DELTAS = "WITH locked AS (SELECT id FROM evaluation \
        WHERE id IN (SELECT evaluation FROM evaluation_shared_build_delta) \
        ORDER BY id FOR NO KEY UPDATE SKIP LOCKED), \
        gone AS (DELETE FROM evaluation_shared_build_delta d USING locked \
        WHERE d.evaluation = locked.id RETURNING \
        d.evaluation, d.named, d.active, d.failed, d.queued, d.building), \
        s AS (SELECT evaluation, sum(named)::int AS named, sum(active)::int AS active, \
              sum(failed)::int AS failed, sum(queued)::int AS queued, \
              sum(building)::int AS building FROM gone GROUP BY evaluation) \
        UPDATE evaluation e SET named_shared_builds = e.named_shared_builds + s.named, \
        active_shared_builds = e.active_shared_builds + s.active, \
        failed_shared_builds = e.failed_shared_builds + s.failed, \
        queued_shared_builds = e.queued_shared_builds + s.queued, \
        building_shared_builds = e.building_shared_builds + s.building \
        FROM s WHERE e.id = s.evaluation",
        params = [],
        tier = Bulk;

    RECOUNT_EVAL_COUNTERS = "WITH locked AS (SELECT id FROM evaluation \
        WHERE id = ANY($1::uuid[]) ORDER BY id FOR NO KEY UPDATE SKIP LOCKED), \
        gone AS (DELETE FROM evaluation_shared_build_delta d USING locked \
        WHERE d.evaluation = locked.id RETURNING d.evaluation), \
        c AS (SELECT e.id, count(bj.id)::int AS named, coalesce(sum(x.active), 0)::int AS active, \
              coalesce(sum(x.failed), 0)::int AS failed, coalesce(sum(x.queued), 0)::int AS queued, \
              coalesce(sum(x.building), 0)::int AS building \
              FROM locked e LEFT JOIN build_job bj ON bj.evaluation = e.id \
              LEFT JOIN derivation_build db ON db.id = bj.derivation_build \
              LEFT JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) x ON db.id IS NOT NULL \
              GROUP BY e.id) \
        UPDATE evaluation e SET named_shared_builds = c.named, active_shared_builds = c.active, \
        failed_shared_builds = c.failed, queued_shared_builds = c.queued, building_shared_builds = c.building \
        FROM c WHERE e.id = c.id AND (e.named_shared_builds, e.active_shared_builds, e.failed_shared_builds, \
        e.queued_shared_builds, e.building_shared_builds) IS DISTINCT FROM \
        (c.named, c.active, c.failed, c.queued, c.building)",
        params = [EvaluationIds(64)],
        tier = Sweep;
}

crate::sql_fn! {
    IN_FLIGHT_EVALS = in_flight_evals_sql,
        params = [];
}

fn in_flight_evals_sql() -> String {
    format!(
        "SELECT id FROM evaluation WHERE status IN ({})",
        crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    )
}

const FOLD_LOCK: i64 = 640;

fn uuids(evaluations: &[EvaluationId]) -> Value {
    evaluations
        .iter()
        .map(|e| e.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

pub async fn eval_counters<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Option<EvalCounters>, DbErr> {
    db.query_one_raw(EVAL_COUNTERS.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .map(|row| EvalCounters::from_row(&row))
        .transpose()
}

pub async fn in_flight_counters<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, EvalCounters>, DbErr> {
    let mut out = HashMap::with_capacity(evaluations.len());
    for chunk in evaluations.chunks(crate::IN_CHUNK_SIZE) {
        for row in db
            .query_all_raw(IN_FLIGHT_COUNTERS.bind([uuids(chunk)]))
            .await?
        {
            let id = EvaluationId::new(row.try_get::<uuid::Uuid>("", "id")?);
            out.insert(id, EvalCounters::from_row(&row)?);
        }
    }

    Ok(out)
}

pub async fn fold_shared_build_deltas<C>(db: &C) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let locked = txn
        .query_one_raw(FOLD_LOCK_TRY.bind([FOLD_LOCK.into()]))
        .await?
        .map(|r| r.try_get::<bool>("", "locked"))
        .transpose()?
        .unwrap_or(false);
    if !locked {
        return Ok(false);
    }

    txn.execute_raw(FOLD_SHARED_BUILD_DELTAS.stmt()).await?;
    txn.commit().await?;
    Ok(true)
}

pub async fn recount_eval_shared_build_counters<C>(db: &C) -> Result<u64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let evaluations: Vec<EvaluationId> = db
        .query_all_raw(IN_FLIGHT_EVALS.stmt())
        .await?
        .iter()
        .map(|r| r.try_get::<uuid::Uuid>("", "id").map(EvaluationId::new))
        .collect::<Result<_, _>>()?;

    recount_evaluations(db, &evaluations).await
}

pub async fn recount_evaluations<C>(db: &C, evaluations: &[EvaluationId]) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut drift = 0;
    for chunk in evaluations.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        txn.execute_raw(FOLD_LOCK_WAIT.bind([FOLD_LOCK.into()]))
            .await?;
        drift += txn
            .execute_raw(RECOUNT_EVAL_COUNTERS.bind([uuids(chunk)]))
            .await?
            .rows_affected();
        txn.commit().await?;
    }

    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    #[test]
    fn membership_matches_the_graph_predicates() {
        let f = gradient_migration::m20261009_000000_build_job_aborted::SHARED_BUILD_COUNTS_FN;
        let alias = "evaluation_shared_build_counts";
        assert!(
            f.contains(&crate::graph::predicates::blocks_evaluation_predicate(
                alias
            )),
            "{f}"
        );
        assert!(
            f.contains(&format!(
                "{alias}.status IN ({})",
                crate::sql::status::build_in(&BuildStatus::REQUEUEABLE)
            )),
            "{f}"
        );
        assert!(
            f.contains(&format!("{alias}.status = {}", BuildStatus::Queued as i32)),
            "{f}"
        );
        assert!(
            f.contains(&format!(
                "{alias}.status = {}",
                BuildStatus::Building as i32
            )),
            "{f}"
        );
    }

    #[test]
    fn the_fold_deletes_what_it_adds_in_one_statement() {
        let sql = FOLD_SHARED_BUILD_DELTAS.text();
        assert!(
            sql.contains("ORDER BY id FOR NO KEY UPDATE SKIP LOCKED), gone AS (DELETE FROM evaluation_shared_build_delta d USING locked"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "UPDATE evaluation e SET named_shared_builds = e.named_shared_builds + s.named"
            ),
            "{sql}"
        );
    }

    #[test]
    fn the_recount_clears_the_ledger_it_counted_past() {
        let sql = RECOUNT_EVAL_COUNTERS.text();
        assert!(
            sql.contains(
                "ORDER BY id FOR NO KEY UPDATE SKIP LOCKED), gone AS (DELETE FROM evaluation_shared_build_delta d USING locked"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("evaluation_shared_build_counts(db.status, db.wanted)"),
            "{sql}"
        );
        assert!(sql.contains("IS DISTINCT FROM"), "{sql}");
    }

    #[tokio::test]
    async fn counters_are_one_read() {
        let row = BTreeMap::from([
            ("named".to_owned(), Value::BigInt(Some(3))),
            ("active".to_owned(), Value::BigInt(Some(1))),
            ("failed".to_owned(), Value::BigInt(Some(0))),
            ("queued".to_owned(), Value::BigInt(Some(1))),
            ("building".to_owned(), Value::BigInt(Some(0))),
        ]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();

        let c = eval_counters(&db, EvaluationId::now_v7())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            c,
            EvalCounters {
                named: 3,
                active: 1,
                queued: 1,
                ..Default::default()
            }
        );
        assert_eq!(db.into_transaction_log().len(), 1);
    }
}
