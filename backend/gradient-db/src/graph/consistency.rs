/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The sweep is the only backstop for the moved columns.
//! `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS = 0` is leaving them with none.

use crate::DbContext;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ConnectionTrait, DbErr, Statement};

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConsistencyReport {
    pub counter_drift: i64,
    pub walk_drift: i64,
    pub runtime_drift: i64,
    pub need_drift: i64,
    pub skipped_moves: i64,
    pub unpromoted_startable: i64,
    pub adopted: i64,
    pub unbacked_trusted_outputs: i64,
    pub wedged_building_evals: i64,
    pub eval_counter_drift: i64,
    pub repair_scope: i64,
}

impl ConsistencyReport {
    pub fn total(&self) -> i64 {
        self.counter_drift
            + self.walk_drift
            + self.runtime_drift
            + self.need_drift
            + self.skipped_moves
            + self.unpromoted_startable
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
            + self.eval_counter_drift
            + self.adopted
    }
}

fn unbacked_trusted_output_count_sql() -> String {
    format!(
        "SELECT count(*) AS n FROM ({}) u",
        crate::caches::demotion::unbacked_trusted_outputs_select()
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
         WHERE ev.status = {building} AND ev.active_shared_builds = 0",
        building = crate::sql::status::eval(EvaluationStatus::Building),
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

pub async fn graph_consistency_report(ctx: &DbContext) -> Result<ConsistencyReport, DbErr> {
    let db = &ctx.worker_db;

    let walk_drift = crate::graph::walk_completeness::recount_walk_completeness(db).await? as i64;

    let runtime_drift =
        crate::graph::runtime_can_start::recount_missing_runtime_deps(db).await? as i64;
    let scope = crate::graph::can_start::can_start_scope(db).await?;
    let fetchable = crate::graph::can_start::repair_fetchable(db, &scope).await?;

    let need_drift = crate::graph::can_start::recount_wanted(db).await? as i64;

    let settled = crate::graph::can_start::settle_skipped(db).await?;
    crate::status::emit_transition_effects(ctx, &settled).await?;

    let repaired = crate::graph::can_start::repair_can_start(db, &scope).await?;
    // Fan-out must follow the order the two statements ran in.
    // A row both moved would otherwise end on the board at the earlier status.
    crate::status::emit_transition_effects(ctx, &repaired.unpromoted).await?;
    crate::status::emit_transition_effects(ctx, &repaired.promoted).await?;

    let adopted = if crate::graph::reachability::pending_orphan_frontier(db).await? {
        let adopted = crate::graph::reachability::adopt_pending_closures(db).await?;
        let mut queued = Vec::new();
        for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
            queued.extend(
                crate::graph::can_start::update_and_settle_need(db, chunk)
                    .await?
                    .changes,
            );
        }
        for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
            queued.extend(crate::graph::can_start::promote(db, chunk).await?);
        }

        crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations()).await?;
        crate::status::emit_transition_effects(ctx, &queued).await?;
        adopted.pairs.len() as i64
    } else {
        0
    };

    let unbacked_trusted_outputs = count(db, UNBACKED_TRUSTED_OUTPUT_COUNT.stmt()).await?;

    let eval_counter_drift =
        crate::evaluations::counters::recount_eval_shared_build_counters(db).await? as i64;
    let wedged_building_evals = count(db, WEDGED_BUILDING_EVALS.stmt()).await?;

    Ok(ConsistencyReport {
        counter_drift: (fetchable + repaired.blocking_deps) as i64,
        walk_drift,
        runtime_drift,
        need_drift,
        skipped_moves: settled.len() as i64,
        unpromoted_startable: repaired.promoted.len() as i64,
        adopted,
        unbacked_trusted_outputs,
        wedged_building_evals,
        eval_counter_drift,
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

    fn scripted(hole: bool) -> sea_orm::DatabaseConnection {
        let n = || vec![BTreeMap::from([("n".to_owned(), Value::BigInt(Some(0)))])];
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let drifted: Vec<BTreeMap<String, Value>> = (0..4)
            .map(|_| {
                BTreeMap::from([
                    ("derivation".to_owned(), Value::from(uuid::Uuid::now_v7())),
                    ("wanted".to_owned(), Value::from(true)),
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

        db.append_query_results([n()])
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(uuid::Uuid::now_v7()),
            )])]])
            .append_query_results([n()])
            .append_exec_results([exec(0), exec(2)])
            .into_connection()
    }

    #[tokio::test]
    async fn the_report_repairs_both_counters_before_it_counts() {
        let (ctx, pool) = crate::test_ctx::ctx(scripted(false)).await;
        let report = graph_consistency_report(&ctx).await.unwrap();
        drop(ctx);

        assert_eq!(
            report.counter_drift, 8,
            "both can-start recounts are reported, not one of them"
        );
        assert_eq!(
            report.repair_scope, 1,
            "the can-start repair's scope is measured too"
        );
        assert_eq!(
            report.need_drift, 4,
            "every shared build the recount rewrote is reported"
        );
        assert_eq!(
            report.walk_drift, 7,
            "the walk recount comes before the need recount"
        );
        assert_eq!(
            report.runtime_drift, 6,
            "the complete-closure recount comes before the can-start repair that reads it"
        );
        assert_eq!(report.adopted, 0);
        assert_eq!(
            report.eval_counter_drift, 2,
            "the evaluation counters are recounted and their drift reported"
        );

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[0].contains("SET LOCAL work_mem")
                && log[1].contains("SET unwalked_inputs = coalesce(c.n, 0)"),
            "the walk recount runs under its own raise, first of all: {log:?}"
        );
        assert!(
            log[2].contains("SET LOCAL work_mem")
                && log[3].contains("SET missing_runtime_deps = coalesce(c.n, 0)"),
            "the complete closure is recounted before anything that reads it: {log:?}"
        );
        assert!(
            log[4].contains("SELECT q.derivation FROM derivation_build q")
                && log[4].contains("q.fetchable AND q.missing_runtime_deps > 0"),
            "the repair scope is read once, contradicting flags included: {log:?}"
        );
        assert!(
            log[5].contains("FOR NO KEY UPDATE") && log[6].contains("SET fetchable"),
            "the flag is repaired under its own ordered lock, before the need walk \
             that stops at a fetchable shared build: {log:?}"
        );
        assert!(
            log[7].contains("SET LOCAL work_mem") && log[8].contains("SET wanted ="),
            "the table-wide need walk takes place under its own raise, on a repaired flag: {log:?}"
        );
        assert!(
            log[9].contains("db.status IN (5, 10) AND db.wanted")
                && log[10].contains("db.status = 0 AND NOT db.wanted"),
            "both Skipped directions read the need this pass corrected: {log:?}"
        );
        assert!(
            log[11].contains("FOR NO KEY UPDATE") && log[12].contains("SET blocking_deps"),
            "the counter recount follows the need recount, in a second locked pass \
             over the same scope: {log:?}"
        );
        assert!(
            log[13].contains("SET status = 0") && log[14].contains("SET status = 1"),
            "the queue is settled against the repaired counters and the corrected need: {log:?}"
        );
        assert!(
            log[15].contains(
                "NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
            ),
            "the naming backstop asks before it walks: {log:?}"
        );
        assert!(
            log[16].contains("SELECT DISTINCT o.hash"),
            "the unbacked alarm follows the repairs: {log:?}"
        );
        assert!(
            log[17].contains("SELECT id FROM evaluation WHERE status IN")
                && log[18].contains("pg_advisory_xact_lock")
                && log[19].contains("DELETE FROM evaluation_shared_build_delta"),
            "the evaluation counters are recounted under the fold's lock: {log:?}"
        );
        assert!(
            log[20].contains("active_shared_builds = 0"),
            "the wedged alarm reads the counters the recount just corrected: {log:?}"
        );
        assert_eq!(log.len(), 21, "{log:?}");
    }

    #[tokio::test]
    async fn the_sweep_adopts_when_a_live_evaluation_reaches_an_unnamed_pending_shared_build() {
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
                && log[19].contains("ORDER BY derivation FOR NO KEY UPDATE")
                && log[20].contains("ON d.derivation = r.derivation ORDER BY r.derivation"),
            "a name gives the closure below it a need, in this pass and not the next: {log:?}"
        );
        assert!(
            log[21].contains("SET status = 1")
                && log[22].contains("graph_version = e.graph_version + 1"),
            "then queue what was named and bump: {log:?}"
        );
        assert!(
            log[23].contains("SELECT DISTINCT o.hash")
                && log[27].contains("active_shared_builds = 0"),
            "the alarms still come last: {log:?}"
        );
    }

    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            counter_drift: 1,
            walk_drift: 9,
            runtime_drift: 10,
            need_drift: 8,
            skipped_moves: 11,
            unpromoted_startable: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
            eval_counter_drift: 6,
            repair_scope: 2000,
            adopted: 2,
        };
        assert_eq!(r.total(), 59);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
