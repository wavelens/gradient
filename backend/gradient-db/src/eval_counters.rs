/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-evaluation anchor counters: the evaluation's folded columns plus its
//! unfolded ledger rows. The triggers of the `evaluation_anchor_counters`
//! migration are the only writers of the ledger; this module folds and recounts.

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, QueryResult, TransactionTrait, Value};
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
        (e.named_anchors + coalesce(sum(d.named), 0))::bigint AS named, \
        (e.active_anchors + coalesce(sum(d.active), 0))::bigint AS active, \
        (e.failed_anchors + coalesce(sum(d.failed), 0))::bigint AS failed, \
        (e.queued_anchors + coalesce(sum(d.queued), 0))::bigint AS queued, \
        (e.building_anchors + coalesce(sum(d.building), 0))::bigint AS building \
        FROM evaluation e LEFT JOIN evaluation_anchor_delta d ON d.evaluation = e.id \
        WHERE e.id = $1 GROUP BY e.id",
        params = [EvaluationId],
        tier = Hot;

    IN_FLIGHT_COUNTERS = "SELECT id, named_anchors::bigint AS named, \
        active_anchors::bigint AS active, failed_anchors::bigint AS failed, \
        queued_anchors::bigint AS queued, building_anchors::bigint AS building \
        FROM evaluation WHERE id = ANY($1::uuid[])",
        params = [EvaluationIds(64)],
        tier = Bulk;

    FOLD_LOCK_TRY = "SELECT pg_try_advisory_xact_lock($1) AS locked",
        params = [Int(640)],
        tier = Hot;

    FOLD_LOCK_WAIT = "SELECT pg_advisory_xact_lock($1)",
        params = [Int(640)],
        tier = Hot;

    FOLD_ANCHOR_DELTAS = "WITH gone AS (DELETE FROM evaluation_anchor_delta RETURNING \
        evaluation, named, active, failed, queued, building), \
        s AS (SELECT evaluation, sum(named)::int AS named, sum(active)::int AS active, \
              sum(failed)::int AS failed, sum(queued)::int AS queued, \
              sum(building)::int AS building FROM gone GROUP BY evaluation) \
        UPDATE evaluation e SET named_anchors = e.named_anchors + s.named, \
        active_anchors = e.active_anchors + s.active, \
        failed_anchors = e.failed_anchors + s.failed, \
        queued_anchors = e.queued_anchors + s.queued, \
        building_anchors = e.building_anchors + s.building \
        FROM s WHERE e.id = s.evaluation",
        params = [],
        tier = Bulk;

    RECOUNT_EVAL_COUNTERS = "WITH gone AS (DELETE FROM evaluation_anchor_delta \
        WHERE evaluation = ANY($1::uuid[]) RETURNING evaluation), \
        c AS (SELECT e.id, count(bj.id)::int AS named, coalesce(sum(x.active), 0)::int AS active, \
              coalesce(sum(x.failed), 0)::int AS failed, coalesce(sum(x.queued), 0)::int AS queued, \
              coalesce(sum(x.building), 0)::int AS building \
              FROM evaluation e LEFT JOIN build_job bj ON bj.evaluation = e.id \
              LEFT JOIN derivation_build db ON db.id = bj.derivation_build \
              LEFT JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) x ON db.id IS NOT NULL \
              WHERE e.id = ANY($1::uuid[]) GROUP BY e.id) \
        UPDATE evaluation e SET named_anchors = c.named, active_anchors = c.active, \
        failed_anchors = c.failed, queued_anchors = c.queued, building_anchors = c.building \
        FROM c WHERE e.id = c.id AND (e.named_anchors, e.active_anchors, e.failed_anchors, \
        e.queued_anchors, e.building_anchors) IS DISTINCT FROM \
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
        crate::status_sql::eval_in(&EvaluationStatus::ACTIVE),
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

/// One evaluation's counters as of now: its folded columns plus the deltas no
/// fold has reached yet. A missing row is an error, not zeros: zero `active`
/// settles an evaluation.
pub async fn eval_counters<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<EvalCounters, DbErr> {
    let row = db
        .query_one_raw(EVAL_COUNTERS.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("{} returned no row", EVAL_COUNTERS.name)))?;

    EvalCounters::from_row(&row)
}

/// The folded counters of `evaluations`, for a caller that has just folded.
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

/// Move the ledger into the evaluation columns. False when another instance
/// holds the fold: its fold covers the same rows.
pub async fn fold_anchor_deltas(db: &DatabaseConnection) -> Result<bool, DbErr> {
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

    txn.execute_raw(FOLD_ANCHOR_DELTAS.stmt()).await?;
    txn.commit().await?;
    Ok(true)
}

/// Recount the counters of every in-flight evaluation from its `build_job`
/// rows and clear its ledger in the same snapshot. Returns the evaluations
/// whose stored counters disagreed.
pub async fn recount_eval_anchor_counters(db: &DatabaseConnection) -> Result<u64, DbErr> {
    let evaluations: Vec<EvaluationId> = db
        .query_all_raw(IN_FLIGHT_EVALS.stmt())
        .await?
        .iter()
        .map(|r| r.try_get::<uuid::Uuid>("", "id").map(EvaluationId::new))
        .collect::<Result<_, _>>()?;

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

    /// The trigger's membership is the graph's, clause for clause: a change to
    /// either predicate without a migration replacing the function fails here.
    #[test]
    fn membership_matches_the_graph_predicates() {
        let f = gradient_migration::m20260923_000002_evaluation_anchor_counters::ANCHOR_COUNTS_FN;
        let alias = "evaluation_anchor_counts";
        assert!(
            f.contains(&crate::graph_sql::blocks_evaluation_predicate(alias)),
            "{f}"
        );
        assert!(
            f.contains(&format!(
                "{alias}.status IN ({})",
                crate::status_sql::build_in(&BuildStatus::REQUEUEABLE)
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

    /// The fold deletes what it adds in one statement, so a delta is either
    /// folded or still in the ledger, never both and never neither.
    #[test]
    fn the_fold_deletes_what_it_adds_in_one_statement() {
        let sql = FOLD_ANCHOR_DELTAS.text();
        assert!(
            sql.starts_with("WITH gone AS (DELETE FROM evaluation_anchor_delta RETURNING"),
            "{sql}"
        );
        assert!(
            sql.contains("UPDATE evaluation e SET named_anchors = e.named_anchors + s.named"),
            "{sql}"
        );
    }

    /// The recount counts and clears the ledger from one snapshot: a delta
    /// committed after it survives and folds on top of the recounted value.
    #[test]
    fn the_recount_clears_the_ledger_it_counted_past() {
        let sql = RECOUNT_EVAL_COUNTERS.text();
        assert!(
            sql.starts_with(
                "WITH gone AS (DELETE FROM evaluation_anchor_delta WHERE evaluation = ANY($1"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("evaluation_anchor_counts(db.status, db.demanded)"),
            "{sql}"
        );
        assert!(sql.contains("IS DISTINCT FROM"), "{sql}");
    }

    /// One read: the evaluation row joined to its own unfolded deltas.
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

        let c = eval_counters(&db, EvaluationId::now_v7()).await.unwrap();
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
