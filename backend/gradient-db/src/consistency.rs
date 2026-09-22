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
use sea_orm::{ConnectionTrait, DbErr, Statement};

/// Counts of graph-invariant violations at one instant.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConsistencyReport {
    /// `fetchable` and `unready_deps` rows rewritten over the pending anchors
    /// and their direct dependencies.
    pub counter_drift: i64,
    /// Walked derivations whose `unwalked_inputs` disagreed with the stubs below them.
    pub walk_drift: i64,
    /// Anchors whose `missing_runtime_deps` disagreed with their runtime edges.
    /// Also the column's backfill: the migration deliberately carries none.
    pub runtime_drift: i64,
    /// Anchors whose `demanded` disagreed with the walk from the entry points.
    pub demand_drift: i64,
    /// Anchors this pass settled to `Skipped` or thawed back out of it. The
    /// backstop for a lost move, and the backfill of the status itself.
    pub skipped_moves: i64,
    /// Promotable anchors found unpromoted, queued by this pass.
    pub unpromoted_ready: i64,
    /// `build_job` rows this pass inserted for pending anchors a live evaluation
    /// reaches and nobody named. A repair, like the drift counts.
    pub adopted: i64,
    /// Outputs of terminal-success producers with no backing artifact. Nothing
    /// repairs this any more: every way it was known to arise is closed where it
    /// happens (see [`crate::cache_storage::unbacked_trusted_outputs_select`]), so
    /// a non-zero count is a bug in one of those, not a queue of work.
    pub unbacked_trusted_outputs: i64,
    /// `Building` evaluations with zero non-terminal anchors left.
    pub wedged_building_evals: i64,
    /// How many anchors the readiness repair locked and recounted. A measurement,
    /// not a violation: each is taken `FOR UPDATE`, twice, against rows every live
    /// graph writer also locks, so the cost is worth seeing on a clean pass too.
    pub repair_scope: i64,
}

impl ConsistencyReport {
    /// Every dimension that warrants a look, summed. The drift counts are rows
    /// this pass already repaired rather than rows still wrong, so a non-zero
    /// total can be a successful self-repair; `repair_scope` is a measurement and
    /// is deliberately not summed.
    pub fn total(&self) -> i64 {
        self.counter_drift
            + self.walk_drift
            + self.runtime_drift
            + self.demand_drift
            + self.skipped_moves
            + self.unpromoted_ready
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
            + self.adopted
    }
}

fn unbacked_trusted_output_count_sql() -> String {
    format!(
        "SELECT count(*) AS n FROM ({}) u",
        crate::cache_storage::unbacked_trusted_outputs_select()
    )
}

crate::sql_fn! {
    UNBACKED_TRUSTED_OUTPUT_COUNT = unbacked_trusted_output_count_sql,
        params = [],
        tier = Sweep;
}

fn wedged_building_evals_sql() -> String {
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
    )
}

crate::sql_fn! {
    WEDGED_BUILDING_EVALS = wedged_building_evals_sql,
        params = [],
        tier = Sweep;
}

async fn count<C: ConnectionTrait>(db: &C, stmt: Statement) -> Result<i64, DbErr> {
    let row = db.query_one_raw(stmt).await?;
    Ok(row
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

/// Recount every maintained column in the order the next one reads it - the walk's
/// subtree bit, anchor wholeness, the flag, demand, then the counter and the queue -
/// and count the violations no recount can repair. Each recount is absolute and
/// table-wide, so the row count it rewrote IS the drift, and a healthy fleet writes
/// nothing.
pub async fn graph_consistency_report(ctx: &DbContext) -> Result<ConsistencyReport, DbErr> {
    let db = &ctx.worker_db;

    // The walk's own bit before the demand it gates: an abandoned walk's parents
    // read complete until this runs, and the prune trusts the column.
    let walk_drift = crate::walk_completeness::recount_walk_completeness(db).await? as i64;

    // Wholeness before the flag that reads it, and the flag before the demand walk
    // that reads it: a stale `fetchable = true` is a settled anchor to every walk,
    // and one that sits two hops below anything pending is nobody else's to repair.
    let runtime_drift = crate::runtime_readiness::recount_missing_runtime_deps(db).await? as i64;
    let scope = crate::readiness::readiness_scope(db).await?;
    let fetchable = crate::readiness::repair_fetchable(db, &scope).await?;

    // Before the queue settles, so it reads a corrected column rather than
    // promoting against a stale demand.
    let demand_drift = crate::readiness::recount_demanded(db).await? as i64;

    // Both directions read the column the recount above just corrected: what
    // nothing wants any more settles, what something wants again wakes.
    let settled = crate::readiness::settle_skipped(db).await?;
    crate::status::emit_transition_effects(ctx, &settled).await;

    let repaired = crate::readiness::repair_readiness(db, &scope).await?;
    // Fan out in the order the two statements ran, or a row both moved ends on
    // the board at the status the earlier statement wrote.
    crate::status::emit_transition_effects(ctx, &repaired.unpromoted).await;
    crate::status::emit_transition_effects(ctx, &repaired.promoted).await;

    // The one naming repair: the events that can leave a pending anchor unnamed
    // each repair it on their own path, and this is the backstop for a lost move.
    let adopted = if crate::reachability::pending_orphan_frontier(db).await? {
        let adopted = crate::reachability::adopt_pending_closures(db).await?;
        // Naming is half of what demand means, as it is on every other adoption
        // path: a name makes the anchor a demander of its own inputs, and without
        // this the closure below what was just named waits for the NEXT sweep's
        // recount to be queueable at all.
        let mut moved = crate::readiness::DemandMoved::default();
        for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
            let chunk_moved = crate::readiness::recompute_demand(db, chunk).await?;
            moved.gained.extend(chunk_moved.gained);
            moved.lost.extend(chunk_moved.lost);
        }

        let mut queued = Vec::new();
        let promotable: Vec<_> = adopted
            .derivations()
            .iter()
            .chain(moved.gained.iter())
            .copied()
            .collect();
        for chunk in promotable.chunks(crate::IN_CHUNK_SIZE) {
            queued.extend(crate::readiness::promote(db, chunk).await?);
        }
        for chunk in moved.lost.chunks(crate::IN_CHUNK_SIZE) {
            queued.extend(crate::readiness::unpromote_ungated(db, chunk).await?);
        }

        crate::bump_graph_version(db, &adopted.evaluations()).await?;
        crate::status::emit_transition_effects(ctx, &queued).await;
        adopted.pairs.len() as i64
    } else {
        0
    };

    let unbacked_trusted_outputs = count(db, UNBACKED_TRUSTED_OUTPUT_COUNT.stmt()).await?;

    let wedged_building_evals = count(db, WEDGED_BUILDING_EVALS.stmt()).await?;

    Ok(ConsistencyReport {
        counter_drift: (fetchable + repaired.unready_deps) as i64,
        walk_drift,
        runtime_drift,
        demand_drift,
        skipped_moves: settled.len() as i64,
        unpromoted_ready: repaired.promoted.len() as i64,
        adopted,
        unbacked_trusted_outputs,
        wedged_building_evals,
        repair_scope: scope.len() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    /// The sweep's statement script: the walk and wholeness recounts under their
    /// own raises, the scope select and the flag repair, the demand recount under
    /// its raise, the queue settle in both directions, the counter repair and the
    /// queue, the naming probe (and the walk it guards when `hole` is set), then
    /// the two read-only alarms.
    fn scripted(hole: bool) -> sea_orm::DatabaseConnection {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let drifted: Vec<BTreeMap<String, Value>> = (0..4)
            .map(|_| {
                BTreeMap::from([
                    ("derivation".to_owned(), Value::from(uuid::Uuid::now_v7())),
                    ("demanded".to_owned(), Value::from(true)),
                ])
            })
            .collect();
        let mut db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(7), exec(0), exec(6)])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(uuid::Uuid::now_v7()),
            )])]])
            .append_exec_results([exec(0), exec(3), exec(0)])
            .append_query_results([drifted])
            .append_query_results([empty.clone(), empty.clone()])
            .append_exec_results([exec(0), exec(5)])
            .append_query_results([empty.clone(), empty.clone()]);
        db = if hole {
            db.append_query_results([vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]])
            .append_exec_results([exec(0)])
            .append_query_results([vec![BTreeMap::from([
                ("evaluation".to_owned(), Value::from(uuid::Uuid::now_v7())),
                ("derivation".to_owned(), Value::from(uuid::Uuid::now_v7())),
            ])]])
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone()])
            .append_exec_results([exec(1)])
        } else {
            db.append_query_results([empty.clone()])
        };

        db.append_query_results([n(), n()]).into_connection()
    }

    /// The repairs are these counters' only backstop, so the report has to
    /// actually run them - the NAR one first, because the readiness recount
    /// reads wholeness. Without this, deleting either leaves the suite green.
    /// The recounts return different row counts so `counter_drift` cannot pass
    /// while carrying only one of the two.
    #[tokio::test]
    async fn the_report_repairs_both_counters_before_it_counts() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(false)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(
            report.counter_drift, 8,
            "both readiness recounts are reported, not one of them"
        );
        assert_eq!(
            report.repair_scope, 1,
            "the readiness repair's scope is measured too"
        );
        assert_eq!(
            report.demand_drift, 4,
            "every anchor the recount rewrote is reported"
        );
        assert_eq!(
            report.walk_drift, 7,
            "the walk recount runs before the demand recount"
        );
        assert_eq!(
            report.runtime_drift, 6,
            "the anchor wholeness recount runs before the readiness repair that reads it"
        );
        assert_eq!(report.adopted, 0);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[0].contains("SET LOCAL work_mem")
                && log[1].contains("SET unwalked_inputs = coalesce(c.n, 0)"),
            "the walk recount runs under its own raise, first of all: {log:?}"
        );
        assert!(
            log[2].contains("SET LOCAL work_mem")
                && log[3].contains("SET missing_runtime_deps = coalesce(c.n, 0)"),
            "wholeness is recounted before anything that reads it: {log:?}"
        );
        assert!(
            log[4].contains("SELECT q.derivation FROM derivation_build q")
                && log[4].contains("q.fetchable AND q.missing_runtime_deps > 0"),
            "the repair scope is read once, contradicting flags included: {log:?}"
        );
        assert!(
            log[5].contains("FOR UPDATE") && log[6].contains("SET fetchable"),
            "the flag is repaired under its own ordered lock, before the demand walk \
             that stops at a fetchable anchor: {log:?}"
        );
        assert!(
            log[7].contains("SET LOCAL work_mem") && log[8].contains("SET demanded ="),
            "the table-wide demand walk runs under its own raise, on a repaired flag: {log:?}"
        );
        assert!(
            log[9].contains("db.status IN (5, 10) AND db.demanded")
                && log[10].contains("db.status = 0 AND NOT db.demanded"),
            "both Skipped directions read the demand this pass corrected: {log:?}"
        );
        assert!(
            log[11].contains("FOR UPDATE") && log[12].contains("SET unready_deps"),
            "the counter recount follows the demand recount, in a second locked pass \
             over the same scope: {log:?}"
        );
        assert!(
            log[13].contains("SET status = 0") && log[14].contains("SET status = 1"),
            "the queue is settled against the repaired counters and the corrected demand: {log:?}"
        );
        assert!(
            log[15].contains(
                "NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
            ),
            "the naming backstop asks before it walks: {log:?}"
        );
        assert!(
            log[16].contains("SELECT DISTINCT o.hash") && log[17].contains("FROM evaluation ev"),
            "the read-only alarms come last: {log:?}"
        );
        assert_eq!(log.len(), 18, "{log:?}");
    }

    /// A pending anchor nobody names below a live evaluation's builder is the one
    /// hole no counter repair can close: the sweep walks, names, recomputes the
    /// demand the new name carries, queues what it named, and reports the rows as
    /// a repair.
    #[tokio::test]
    async fn the_sweep_adopts_when_a_live_evaluation_reaches_an_unnamed_pending_anchor() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(true)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(report.adopted, 1);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[15].contains("LIMIT 1")
                && log[16].contains("SET LOCAL work_mem")
                && log[17].contains("INSERT INTO build_job"),
            "the probe guards the walk that names: {log:?}"
        );
        assert!(
            log[18].contains("SET LOCAL work_mem")
                && log[19].contains("ORDER BY derivation FOR UPDATE")
                && log[20].contains("FROM region r ORDER BY r.derivation"),
            "a name gives the closure below it demand, in this pass and not the next: {log:?}"
        );
        assert!(
            log[21].contains("SET status = 1")
                && log[22].contains("graph_version = e.graph_version + 1"),
            "then queue what was named and bump: {log:?}"
        );
        assert!(
            log[23].contains("SELECT DISTINCT o.hash") && log[24].contains("FROM evaluation ev"),
            "the read-only alarms still come last: {log:?}"
        );
    }

    /// Every violation counts toward the warning, and the one measurement does
    /// not: `repair_scope` is how much work the repair did, not something wrong.
    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            counter_drift: 1,
            walk_drift: 9,
            runtime_drift: 10,
            demand_drift: 8,
            skipped_moves: 11,
            unpromoted_ready: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            repair_scope: 2000,
            adopted: 2,
        };
        assert_eq!(r.total(), 53);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
