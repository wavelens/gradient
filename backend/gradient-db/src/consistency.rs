/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build-graph invariant assertions: one repair pass, then counts. Counts
//! violations of the invariants the dispatch/promotion gates trust, so a dead zone
//! surfaces as a warning metric instead of a user-reported stuck evaluation. Reuses the very
//! gate SQL the reconciler maintains, so a non-zero count means "the healing
//! pipeline is not converging", never "the checker disagrees with the gates".
//! Transient non-zero counts between a transition and this pass are expected;
//! persistent counts are the alert. There is no reconcile tick to wait for: the
//! event that changes a counter moves it, and this sweep is the only backstop.
//!
//! One dimension is not read-only: the NAR reference counter is moved rather
//! than derived, so nothing else would ever notice a lost move. This pass
//! repairs it in place over the paths that gate pending anchors, and reports
//! how many rows it had to correct. That makes the sweep the counter's only
//! backstop, so `GRADIENT_GRAPH_CONSISTENCY_INTERVAL = 0` leaves it with none.
//! The counters BELOW zero are counted separately and table-wide: the repair is
//! bounded to the gating paths, so a row driven negative outside them is exactly
//! the state the design calls unrecoverable and the drift count cannot see it.

use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseTransaction, DbErr, Statement, TransactionTrait,
};

/// Counts of graph-invariant violations at one instant.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConsistencyReport {
    /// Anchors trusted `closure_complete` whose gate no longer holds.
    pub stale_closure_complete: i64,
    /// Anchors trusted `drv_closure_cached` whose gate no longer holds.
    pub stale_drv_closure_cached: i64,
    /// `Created` anchors that pass the full promotion predicate yet sit unpromoted.
    pub unpromoted_ready: i64,
    /// Outputs of terminal-success producers with no backing artifact.
    pub unbacked_trusted_outputs: i64,
    /// `Building` evaluations with zero non-terminal anchors left.
    pub wedged_building_evals: i64,
    /// `cached_path` rows whose reference counter this pass had to repair.
    pub nar_counter_drift: i64,
    /// `cached_path` rows whose counter sits below zero: a ripple that ran twice.
    /// No gate can read such a row as whole again, and only a repair pass over a
    /// path a pending anchor gates on rescues one, so a persistent count here is
    /// the one state the counter's design calls unrecoverable.
    pub negative_reference_counters: i64,
    /// How many paths the repair visited this pass. A measurement, not a
    /// violation: it is the size of an unbounded select, reported so the cost of
    /// the one recurring scan this pass adds is visible before it is bounded.
    pub gating_paths: i64,
}

impl ConsistencyReport {
    /// Every dimension that warrants a look, summed. `nar_counter_drift` counts
    /// rows this pass already repaired rather than rows still wrong, so a
    /// non-zero total can be a successful self-repair; `gating_paths` is a
    /// measurement and is deliberately not summed.
    pub fn total(&self) -> i64 {
        self.stale_closure_complete
            + self.stale_drv_closure_cached
            + self.unpromoted_ready
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
            + self.nar_counter_drift
            + self.negative_reference_counters
    }
}

async fn count<C: ConnectionTrait>(db: &C, sql: String) -> Result<i64, DbErr> {
    let row = db
        .query_one_raw(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

/// Repair the NAR reference counter over the paths the pending anchors gate on,
/// then count every invariant violation the gates could act on right now. The
/// repair runs first because two of those counts embed gates that read the
/// counter, so a drifted row would otherwise inflate a count this very pass fixes.
///
/// The negative-counter count is table-wide on purpose: the repair is bounded to
/// the gating paths, so drift outside them is invisible to `nar_counter_drift` by
/// construction. `gating_paths` reports the size of that bounded set, which is an
/// unbounded select today (#591 rewrites what the sweep reads, so it is measured
/// now and bounded there rather than with a rotation scheme thrown away next PR).
pub async fn graph_consistency_report<C>(db: &C) -> Result<ConsistencyReport, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let closure_gate = crate::promotion::closure_complete_gate();
    let drv_gate = crate::promotion::drv_closure_cached_gate();
    let deps_ready = crate::graph_sql::deps_ready_predicate("db");
    let walked = crate::graph_sql::walked_predicate("db");
    let unbacked = crate::cache_storage::unbacked_trusted_outputs_select();

    let gating = db
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            crate::nar_closure::gating_paths(),
        ))
        .await?
        .into_iter()
        .map(|r| r.try_get::<String>("", "hash"))
        .collect::<Result<Vec<_>, _>>()?;
    let nar_counter_drift = crate::nar_closure::repair_counters_for(db, &gating).await? as i64;
    let negative_reference_counters = count(
        db,
        "SELECT count(*) AS n FROM cached_path WHERE missing_references < 0".to_owned(),
    )
    .await?;

    let stale_closure_complete = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.closure_complete AND NOT ({closure_gate})"
        ),
    )
    .await?;

    let stale_drv_closure_cached = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.drv_closure_cached AND NOT ({drv_gate})"
        ),
    )
    .await?;

    let unpromoted_ready = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.status = {created} \
               AND {walked} \
               AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) \
               AND (db.substitutable OR ({deps_ready}))",
            created = status_sql::build(BuildStatus::Created),
        ),
    )
    .await?;

    let unbacked_trusted_outputs =
        count(db, format!("SELECT count(*) AS n FROM ({unbacked}) u")).await?;

    let wedged_building_evals = count(
        db,
        format!(
            "SELECT count(*) AS n FROM evaluation ev \
             WHERE ev.status = {building} \
               AND NOT EXISTS ( \
                 SELECT 1 FROM build_job bj \
                 JOIN derivation_build db ON db.derivation = bj.derivation \
                 WHERE bj.evaluation = ev.id AND db.status IN ({non_terminal}))",
            building = status_sql::eval(EvaluationStatus::Building),
            non_terminal = status_sql::build_in(&[
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
                BuildStatus::FailedTransient,
            ]),
        ),
    )
    .await?;

    Ok(ConsistencyReport {
        stale_closure_complete,
        stale_drv_closure_cached,
        unpromoted_ready,
        unbacked_trusted_outputs,
        wedged_building_evals,
        nar_counter_drift,
        negative_reference_counters,
        gating_paths: gating.len() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    /// The repair is the counter's only backstop, so the report has to actually
    /// run it - and run it before the counts, two of which embed gates that read
    /// the counter. Without this, deleting those lines leaves the suite green.
    #[tokio::test]
    async fn the_report_repairs_the_gating_paths_before_it_counts() {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::from("h".to_owned()),
            )])]])
            .append_query_results([n(), n(), n(), n(), n(), n()])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                },
            ])
            .into_connection();

        let report = graph_consistency_report(&db).await.unwrap();

        assert_eq!(
            report.nar_counter_drift, 2,
            "the repaired rows are reported"
        );
        assert_eq!(report.gating_paths, 1, "the repair's scope is measured");
        let log = crate::pool::statements(db.into_transaction_log());
        assert!(
            log[0].contains("SELECT d.hash FROM derivation d")
                && log[0].contains("JOIN derivation_dependency e ON e.dependency = o.derivation"),
            "the gating select comes first: {log:?}"
        );
        assert!(
            log[1].contains("FOR UPDATE")
                && log[2].contains("UPDATE cached_path cp SET missing_references"),
            "the repair locks, recounts, and runs before any count reads the counter: {log:?}"
        );
        assert!(
            log[3].contains("missing_references < 0"),
            "a counter below zero is unrecoverable, so it must be counted: {log:?}"
        );
    }

    /// Every violation counts toward the warning, and the one measurement does
    /// not: `gating_paths` is how much work the repair did, not something wrong.
    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            stale_closure_complete: 1,
            stale_drv_closure_cached: 2,
            unpromoted_ready: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            nar_counter_drift: 6,
            negative_reference_counters: 7,
            gating_paths: 1000,
        };
        assert_eq!(r.total(), 28);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
