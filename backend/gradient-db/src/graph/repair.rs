/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The steps are sharing the graph writer's transaction.
//! The first failure is ending the pass and failing the transition.

use crate::DbContext;
use crate::status::{TransitionChange, emit_transition_effects};
use gradient_types::EvaluationId;
use sea_orm::DbErr;
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RepairScope {
    Eval(EvaluationId),
    Unstick(EvaluationId),
    Retry(EvaluationId),
}

impl RepairScope {
    pub fn evaluation(&self) -> EvaluationId {
        match self {
            RepairScope::Eval(id) | RepairScope::Unstick(id) | RepairScope::Retry(id) => *id,
        }
    }
}

#[derive(Debug, Default)]
pub struct RepairReport {
    pub thawed: u64,
    pub cached_repaired: usize,
    pub adopted: usize,
    pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}

impl RepairReport {
    pub fn is_noop(&self) -> bool {
        self.thawed == 0
            && self.cached_repaired == 0
            && self.adopted == 0
            && self.dependency_failed.is_empty()
            && self.promoted.is_empty()
    }
}

pub async fn repair_build_graph(
    ctx: &DbContext,
    scope: RepairScope,
) -> Result<RepairReport, DbErr> {
    let db = &ctx.worker_db;
    let evaluation = scope.evaluation();
    let mut report = RepairReport::default();

    let thawed = crate::graph::promotion::requeue_failed_closure(db, scope).await?;
    report.thawed = thawed.len() as u64;
    emit_transition_effects(ctx, &thawed).await?;

    let cached =
        crate::graph::promotion::repair_cached_shared_builds_for_eval(db, evaluation).await?;
    report.cached_repaired = cached.len();
    emit_transition_effects(ctx, &cached).await?;
    let derivations: Vec<_> = cached.iter().map(|c| c.derivation).collect();
    let advanced = crate::graph::can_start::advance_fetchable(db, &derivations).await?;
    emit_transition_effects(ctx, &advanced).await?;

    report.dependency_failed =
        crate::graph::promotion::repair_dependency_failed(db, evaluation).await?;
    emit_transition_effects(ctx, &report.dependency_failed).await?;

    let adopted = crate::graph::reachability::adopt_pending_closure(db, evaluation).await?;
    report.adopted = adopted.pairs.len();
    for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
        let settled = crate::graph::can_start::update_and_settle_need(db, chunk).await?;
        emit_transition_effects(ctx, &settled.changes).await?;
    }
    crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations()).await?;

    report.promoted = crate::graph::can_start::promote_closure(db, evaluation).await?;
    emit_transition_effects(ctx, &report.promoted).await?;

    if !report.is_noop() {
        debug!(
            ?scope,
            thawed = report.thawed,
            cached_repaired = report.cached_repaired,
            adopted = report.adopted,
            dependency_failed = report.dependency_failed.len(),
            promoted = report.promoted.len(),
            "graph repair made progress"
        );
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_heal_adopts_the_closure_before_it_promotes_it() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let eval = EvaluationId::now_v7();
        let d = gradient_types::DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let exec = |rows_affected| MockExecResult {
            last_insert_id: 0,
            rows_affected,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone()])
            .append_exec_results([exec(0)])
            .append_query_results([empty.clone()])
            .append_exec_results([exec(0)])
            .append_query_results([vec![BTreeMap::from([
                ("evaluation".to_owned(), Value::from(eval.into_inner())),
                ("derivation".to_owned(), Value::from(d.into_inner())),
            ])]])
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone()])
            .append_exec_results([exec(1)])
            .append_exec_results([exec(0)])
            .append_query_results([empty])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let report = repair_build_graph(&ctx, RepairScope::Eval(eval))
            .await
            .expect("a converging heal succeeds");
        drop(ctx);

        assert_eq!(report.adopted, 1);
        assert!(!report.is_noop(), "an adoption is progress");
        let log = crate::pool::statements(pool.into_transaction_log());
        let adopt = log
            .iter()
            .position(|s| s.contains("INSERT INTO build_job"))
            .expect("the heal adopts");
        let bump = log
            .iter()
            .position(|s| s.contains("graph_version = e.graph_version + 1"))
            .expect("the adoption bumps the graph version");
        let promote = log
            .iter()
            .position(|s| s.contains("SET status = 1"))
            .expect("the heal promotes the closure");
        assert!(
            log[adopt].contains("WHERE bj.evaluation = $1"),
            "scoped to the healed evaluation: {log:?}"
        );
        let need = log
            .iter()
            .position(|s| s.contains("ON d.derivation = r.derivation ORDER BY r.derivation"))
            .expect("the adoption updates what it named");
        assert!(
            adopt < need && need < bump && bump < promote,
            "adopt, update what a name gave a need to, bump, then promote: {log:?}"
        );
        assert!(
            log[..adopt].iter().any(|s| {
                s.contains("db.status IN (4, 5, 6, 9)") && !s.contains("deterministic_blocked")
            }),
            "a fresh intent's thaw runs before the adoption and blocks nothing: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_heal_fails_the_repair_and_starts_nothing_after_it() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_errors([DbErr::Custom("deadlock detected".into())])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let outcome = repair_build_graph(&ctx, RepairScope::Eval(EvaluationId::now_v7())).await;
        drop(ctx);

        assert!(outcome.is_err(), "{outcome:?}");
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.contains("SET status = 1")),
            "no later heal ran: {log:?}"
        );
    }

    #[tokio::test]
    async fn an_unstick_keeps_a_reproducible_failure_out_of_its_thaw() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let eval = EvaluationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let exec = |rows_affected| MockExecResult {
            last_insert_id: 0,
            rows_affected,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(0), exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone(), empty.clone(), empty.clone()])
            .append_exec_results([exec(0)])
            .append_query_results([empty])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let report = repair_build_graph(&ctx, RepairScope::Unstick(eval))
            .await
            .expect("a converged graph heals nothing");
        drop(ctx);

        assert!(report.is_noop());
        let log = crate::pool::statements(pool.into_transaction_log());
        let thaw = log
            .iter()
            .find(|s| s.contains("db.status IN (4, 5, 6, 9)"))
            .expect("the unstick thaws");
        assert!(
            thaw.contains("NOT IN (SELECT derivation FROM deterministic_blocked)"),
            "{thaw}"
        );
    }
}
