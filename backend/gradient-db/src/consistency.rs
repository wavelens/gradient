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
    /// Anchors whose `demanded` disagreed with the walk from the entry points.
    pub demand_drift: i64,
    /// Promotable anchors found unpromoted, queued by this pass.
    pub unpromoted_ready: i64,
    /// `build_job` rows this pass inserted for pending anchors a live evaluation
    /// reaches and nobody named. A repair, like the drift counts.
    pub adopted: i64,
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
    /// How many paths the NAR repair visited this pass. A measurement, not a
    /// violation: it is the size of an unbounded select, reported so the cost of
    /// the recurring scans this pass adds is visible before they are bounded.
    pub gating_paths: i64,
    /// How many anchors the readiness repair locked and recounted. Fewer rows
    /// than [`Self::gating_paths`] but the costlier scan: each is taken
    /// `FOR UPDATE`, twice, against rows every live graph writer also locks.
    pub repair_scope: i64,
}

impl ConsistencyReport {
    /// Every dimension that warrants a look, summed. `nar_counter_drift` counts
    /// rows this pass already repaired rather than rows still wrong, so a
    /// non-zero total can be a successful self-repair; the two scope sizes are
    /// measurements and are deliberately not summed.
    pub fn total(&self) -> i64 {
        self.counter_drift
            + self.demand_drift
            + self.unpromoted_ready
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
            + self.nar_counter_drift
            + self.negative_reference_counters
            + self.adopted
    }
}

crate::sql! {
    NEGATIVE_REFERENCE_COUNTERS = "SELECT count(*) AS n FROM cached_path WHERE missing_references < 0",
        params = [],
        tier = Sweep;
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

crate::sql_fn! {
    GATING_PATHS = crate::nar_closure::gating_paths,
        params = [],
        tier = Sweep;
}

async fn count<C: ConnectionTrait>(db: &C, stmt: Statement) -> Result<i64, DbErr> {
    let row = db.query_one_raw(stmt).await?;
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

    let gating = db
        .query_all_raw(GATING_PATHS.stmt())
        .await?
        .into_iter()
        .map(|r| r.try_get::<String>("", "hash"))
        .collect::<Result<Vec<_>, _>>()?;
    let nar_counter_drift = crate::nar_closure::repair_counters_for(db, &gating).await? as i64;
    let negative_reference_counters = count(db, NEGATIVE_REFERENCE_COUNTERS.stmt()).await?;

    // Before the readiness repair, so the queue settle that follows reads a
    // corrected column rather than promoting against a stale demand.
    let demand_drift = crate::readiness::recount_demanded(db).await? as i64;

    let repaired = crate::readiness::repair_pending(db).await?;
    // Fan out in the order the two statements ran, or a row both moved ends on
    // the board at the status the earlier statement wrote.
    crate::status::emit_transition_effects(ctx, &repaired.unpromoted).await;
    crate::status::emit_transition_effects(ctx, &repaired.promoted).await;

    // The one naming repair: the events that can leave a pending anchor unnamed
    // each repair it on their own path, and this is the backstop for a lost move.
    let adopted = if crate::reachability::pending_orphan_frontier(db).await? {
        let adopted = crate::reachability::adopt_pending_closures(db).await?;
        let mut queued = Vec::new();
        for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
            queued.extend(crate::readiness::promote(db, chunk).await?);
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
        counter_drift: (repaired.fetchable + repaired.unready_deps) as i64,
        demand_drift,
        unpromoted_ready: repaired.promoted.len() as i64,
        adopted,
        unbacked_trusted_outputs,
        wedged_building_evals,
        nar_counter_drift,
        negative_reference_counters,
        gating_paths: gating.len() as i64,
        repair_scope: repaired.scope as i64,
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

    /// The sweep's statement script: the gating select, the NAR repair, the
    /// readiness repair, the queue settle, the naming probe (and the walk it
    /// guards when `hole` is set), then the two read-only alarms.
    fn scripted(hole: bool) -> sea_orm::DatabaseConnection {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let mut db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::from("h".to_owned()),
            )])]])
            .append_exec_results([exec(0), exec(2)])
            .append_query_results([n()])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(uuid::Uuid::now_v7()),
            )])]])
            .append_exec_results([exec(0), exec(3), exec(0), exec(5)])
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
            .append_query_results([empty.clone()])
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
            report.nar_counter_drift, 2,
            "the repaired paths are reported"
        );
        assert_eq!(report.gating_paths, 1, "the NAR repair's scope is measured");
        assert_eq!(
            report.counter_drift, 8,
            "both readiness recounts are reported, not one of them"
        );
        assert_eq!(
            report.repair_scope, 1,
            "the readiness repair's scope is measured too"
        );
        assert_eq!(report.adopted, 0);

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
            log[5].contains("FOR UPDATE") && log[6].contains("SET fetchable"),
            "the fetchable recount runs under its own ordered lock: {log:?}"
        );
        assert!(
            log[7].contains("FOR UPDATE") && log[8].contains("SET unready_deps"),
            "and the counter recount after it, in a second locked pass: {log:?}"
        );
        assert!(
            log[9].contains("SET status = 0") && log[10].contains("SET status = 1"),
            "the queue is settled against the repaired counters: {log:?}"
        );
        assert!(
            log[11].contains(
                "NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
            ),
            "the naming backstop asks before it walks: {log:?}"
        );
        assert!(
            log[12].contains("SELECT DISTINCT o.hash") && log[13].contains("FROM evaluation ev"),
            "the read-only alarms come last: {log:?}"
        );
        assert_eq!(log.len(), 14, "{log:?}");
    }

    /// A pending anchor nobody names below a live evaluation's builder is the one
    /// hole no counter repair can close: the sweep walks, names, queues what it
    /// named, and reports the rows as a repair.
    #[tokio::test]
    async fn the_sweep_adopts_when_a_live_evaluation_reaches_an_unnamed_pending_anchor() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(true)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(report.adopted, 1);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[11].contains("LIMIT 1")
                && log[12].contains("SET LOCAL work_mem")
                && log[13].contains("INSERT INTO build_job")
                && log[14].contains("SET status = 1")
                && log[15].contains("graph_version = e.graph_version + 1"),
            "probe, walk, queue what was named, bump: {log:?}"
        );
        assert!(
            log[16].contains("SELECT DISTINCT o.hash") && log[17].contains("FROM evaluation ev"),
            "the read-only alarms still come last: {log:?}"
        );
    }

    /// Every violation counts toward the warning, and the one measurement does
    /// not: `gating_paths` is how much work the repair did, not something wrong.
    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            counter_drift: 1,
            demand_drift: 8,
            unpromoted_ready: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            nar_counter_drift: 6,
            negative_reference_counters: 7,
            gating_paths: 1000,
            repair_scope: 2000,
            adopted: 2,
        };
        assert_eq!(r.total(), 36);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
