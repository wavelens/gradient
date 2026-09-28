/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one graph reconciler: the heals for state no event can reach, run at an
//! evaluation's stream completion (`Eval`) and when a building evaluation is
//! graph-stuck (`Unstick`). Both scopes run the same steps. No scope runs on a tick
//! and nothing here is a fixpoint; every counter is moved by the event that changes
//! it, and [`crate::readiness::repair_readiness`] is the backstop for a move that
//! was lost.
//!
//! Both scopes name an evaluation, so every statement is bounded to that
//! evaluation's dependency closure. The steps share the graph actor's transaction,
//! so the first failure ends the pass and fails the transition.

use crate::DbContext;
use crate::status::{TransitionChange, emit_transition_effects};
use gradient_types::EvaluationId;
use sea_orm::DbErr;
use tracing::debug;

/// What slice of the graph to heal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReconcileScope {
    /// A fresh intent: an evaluation just flushed its graph, or a restart took the
    /// previous evaluation's names over. Thaw every failed anchor in its closure, a
    /// reproducible builder exit included, settle the anchors whose outputs are
    /// already whole, fail the dependents of a failure that stays, name the open
    /// anchors it reaches, promote the closure.
    Eval(EvaluationId),
    /// A wedged evaluation healing itself: the same steps, except that the thaw
    /// leaves a reproducible failure and the subtree it poisons alone, since the
    /// intent that already failed on it is the one asking again.
    Unstick(EvaluationId),
}

impl ReconcileScope {
    pub fn evaluation(&self) -> EvaluationId {
        match self {
            ReconcileScope::Eval(id) | ReconcileScope::Unstick(id) => *id,
        }
    }
}

/// What one reconciliation pass changed. All-zero on a converged graph.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub thawed: u64,
    pub cached_reconciled: usize,
    pub adopted: usize,
    pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}

impl ReconcileReport {
    pub fn is_noop(&self) -> bool {
        self.thawed == 0
            && self.cached_reconciled == 0
            && self.adopted == 0
            && self.dependency_failed.is_empty()
            && self.promoted.is_empty()
    }
}

/// Run the healing pipeline for `scope`. Effects (the graph version, board events,
/// CI checks, eval finalization) fan out through the one emitter for every anchor
/// a step moved, so a reconciliation can never move an anchor without its
/// consequences.
pub async fn reconcile_build_graph(
    ctx: &DbContext,
    scope: ReconcileScope,
) -> Result<ReconcileReport, DbErr> {
    let db = &ctx.worker_db;
    let evaluation = scope.evaluation();
    let mut report = ReconcileReport::default();

    let thawed = crate::promotion::requeue_failed_closure(db, scope).await?;
    report.thawed = thawed.len() as u64;
    emit_transition_effects(ctx, &thawed).await;

    // Cache presence is the ground truth for "is this built": anchors whose
    // outputs all exist re-complete even after a requeue, cascade or demote, and
    // the anchors they just made servable advance their dependents' counters.
    let cached = crate::promotion::reconcile_cached_anchors_for_eval(db, evaluation).await?;
    report.cached_reconciled = cached.len();
    emit_transition_effects(ctx, &cached).await;
    let derivations: Vec<_> = cached.iter().map(|c| c.derivation).collect();
    let advanced = crate::readiness::advance_fetchable(db, &derivations).await?;
    emit_transition_effects(ctx, &advanced).await;

    // Failure-side backstop, paired with the thaw above: fail every non-terminal
    // anchor in the closure reachable from a terminal failure, including the
    // victims the thaw just re-created.
    report.dependency_failed =
        crate::promotion::reconcile_dependency_failed(db, evaluation).await?;
    emit_transition_effects(ctx, &report.dependency_failed).await;

    // A pruned interior in this closure that a thaw or a reset left with no name
    // fails the gate the promote embeds; this evaluation names it first. Naming is
    // half of what demand means, so an adoption creates it the way a thaw does.
    let adopted = crate::reachability::adopt_pending_closure(db, evaluation).await?;
    report.adopted = adopted.pairs.len();
    for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
        crate::readiness::recompute_demand(db, chunk).await?;
    }
    crate::bump_graph_version(db, &adopted.evaluations()).await?;

    report.promoted = crate::readiness::promote_closure(db, evaluation).await?;
    emit_transition_effects(ctx, &report.promoted).await;

    if !report.is_noop() {
        debug!(
            ?scope,
            thawed = report.thawed,
            cached_reconciled = report.cached_reconciled,
            adopted = report.adopted,
            dependency_failed = report.dependency_failed.len(),
            promoted = report.promoted.len(),
            "graph reconciliation made progress"
        );
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The heal names what it thawed and reset before it promotes: a pruned
    /// interior in this closure that no evaluation names any more would otherwise
    /// fail the gate the promote embeds, and the heal would loop on it forever.
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
            // the thaw and the cache reconcile each open a walk and move nothing
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone(), empty.clone()])
            // the dependency-failed sweep opens a walk and moves nothing
            .append_exec_results([exec(0)])
            .append_query_results([empty.clone()])
            // the adoption opens a walk and takes one name on
            .append_exec_results([exec(0)])
            .append_query_results([vec![BTreeMap::from([
                ("evaluation".to_owned(), Value::from(eval.into_inner())),
                ("derivation".to_owned(), Value::from(d.into_inner())),
            ])]])
            // the adoption recomputes demand below what it named, raised and locked
            .append_exec_results([exec(0), exec(0)])
            .append_query_results([empty.clone()])
            // the adopting evaluation's graph version
            .append_exec_results([exec(1)])
            // the closure promote opens a walk and finds nothing ready yet
            .append_exec_results([exec(0)])
            .append_query_results([empty])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let report = reconcile_build_graph(&ctx, ReconcileScope::Eval(eval))
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
        let demand = log
            .iter()
            .position(|s| s.contains("ON d.derivation = r.derivation ORDER BY r.derivation"))
            .expect("the adoption recomputes what it named");
        assert!(
            adopt < demand && demand < bump && bump < promote,
            "adopt, recompute what a name gave demand to, bump, then promote: {log:?}"
        );
        assert!(
            log[..adopt].iter().any(|s| {
                s.contains("db.status IN (4, 5, 6, 9)") && !s.contains("deterministic_blocked")
            }),
            "a fresh intent's thaw runs before the adoption and blocks nothing: {log:?}"
        );
    }

    /// Every heal runs in the graph actor's one transaction, which Postgres aborts
    /// at the first failed statement: a heal that logged and went on would leave
    /// the actor's COMMIT to roll the whole transition back in silence and would
    /// hide a deadlock from its retry.
    #[tokio::test]
    async fn a_failed_heal_fails_the_reconciliation_and_runs_nothing_after_it() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_errors([DbErr::Custom("deadlock detected".into())])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let outcome =
            reconcile_build_graph(&ctx, ReconcileScope::Eval(EvaluationId::now_v7())).await;
        drop(ctx);

        assert!(outcome.is_err(), "{outcome:?}");
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.contains("SET status = 1")),
            "no later heal ran: {log:?}"
        );
    }

    /// An unstick is the same intent asking again, so its thaw keeps a reproducible
    /// failure and the subtree it poisons out; otherwise a permanent failure inside
    /// a runtime closure is rebuilt on every sweep.
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
        let report = reconcile_build_graph(&ctx, ReconcileScope::Unstick(eval))
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
