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
//! Two dimensions are not read-only: the readiness counters and the NAR
//! reference counter are moved rather than derived, so nothing else would ever
//! notice a lost move. This pass recomputes both over the scope that gates
//! progress and repairs them in place, so the counts it reports for them are
//! what was repaired. That makes the sweep those counters' only backstop, so
//! `GRADIENT_GRAPH_CONSISTENCY_INTERVAL = 0` leaves them with none.
//! The counters BELOW zero are counted separately and table-wide: the repair is
//! bounded to the gating paths, so a row driven negative outside them is exactly
//! the state the design calls unrecoverable and the drift count cannot see it.

use crate::{DbContext, status_sql};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

/// Counts of graph-invariant violations at one instant.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConsistencyReport {
    /// `fetchable` and `unready_deps` rows rewritten over the pending anchors
    /// and their direct dependencies.
    pub counter_drift: i64,
    /// Promotable anchors found unpromoted, queued by this pass.
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
        self.counter_drift
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

/// Repair the NAR reference counter over the paths the pending anchors gate on
/// and the readiness counters over the anchors themselves, then count the
/// violations no counter can repair. The NAR repair runs first because the
/// readiness recount reads wholeness, so a drifted path would otherwise teach the
/// anchors a count this very pass fixes.
///
/// The negative-counter count is table-wide on purpose: the repair is bounded to
/// the gating paths, so drift outside them is invisible to `nar_counter_drift` by
/// construction. `gating_paths` reports the size of that bounded set.
pub async fn graph_consistency_report(ctx: &DbContext) -> Result<ConsistencyReport, DbErr> {
    let db = &ctx.worker_db;
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

    let repaired = crate::readiness::repair_pending(db).await?;
    crate::status::emit_transition_effects(ctx, &repaired.promoted).await;
    crate::status::emit_transition_effects(ctx, &repaired.unpromoted).await;

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
        counter_drift: (repaired.fetchable + repaired.unready_deps) as i64,
        unpromoted_ready: repaired.promoted.len() as i64,
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

    /// The repairs are these counters' only backstop, so the report has to
    /// actually run them - the NAR one first, because the readiness recount
    /// reads wholeness. Without this, deleting either leaves the suite green.
    #[tokio::test]
    async fn the_report_repairs_both_counters_before_it_counts() {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::from("h".to_owned()),
            )])]])
            .append_query_results([n()])
            .append_query_results([empty.clone(), empty.clone(), empty])
            .append_query_results([n(), n()])
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

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(
            report.nar_counter_drift, 2,
            "the repaired rows are reported"
        );
        assert_eq!(report.gating_paths, 1, "the repair's scope is measured");
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[0].contains("SELECT d.hash FROM derivation d")
                && log[0].contains("JOIN derivation_dependency e ON e.dependency = o.derivation"),
            "the gating select comes first: {log:?}"
        );
        assert!(
            log[1].contains("FOR UPDATE")
                && log[2].contains("UPDATE cached_path cp SET missing_references"),
            "the NAR repair locks, recounts, and runs before anything reads wholeness: {log:?}"
        );
        assert!(
            log[3].contains("missing_references < 0"),
            "a counter below zero is unrecoverable, so it must be counted: {log:?}"
        );
        assert!(
            log[4].contains("SELECT q.derivation FROM derivation_build q"),
            "the readiness repair materialises its scope: {log:?}"
        );
        assert!(
            log[5].contains("SET status = 0") && log[6].contains("SET status = 1"),
            "the queue is settled against the repaired counters: {log:?}"
        );
        assert!(
            log[7].contains("SELECT DISTINCT o.hash") && log[8].contains("FROM evaluation ev"),
            "the read-only alarms come last: {log:?}"
        );
    }

    /// Every violation counts toward the warning, and the one measurement does
    /// not: `gating_paths` is how much work the repair did, not something wrong.
    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            counter_drift: 1,
            unpromoted_ready: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            nar_counter_drift: 6,
            negative_reference_counters: 7,
            gating_paths: 1000,
        };
        assert_eq!(r.total(), 26);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
