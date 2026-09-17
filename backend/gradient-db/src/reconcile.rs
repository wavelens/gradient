/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one graph reconciler: the heals for state no event can reach, run at an
//! evaluation's stream completion (`Eval`) and when a building evaluation is
//! graph-stuck (`Unstick`). No scope runs on a tick and nothing here is a
//! fixpoint; every counter is moved by the event that changes it, and
//! [`crate::readiness::repair_pending`] is the backstop for a move that was lost.
//!
//! Both scopes name an evaluation, so every statement is bounded to that
//! evaluation's dependency closure. Each step is logged-and-continued on error: a
//! failing heal must never block the remaining heals.

use crate::DbContext;
use crate::status::{TransitionChange, emit_transition_effects};
use gradient_types::EvaluationId;
use tracing::{debug, error};

/// What slice of the graph to heal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReconcileScope {
    /// An evaluation just flushed its graph: thaw the terminal-failed anchors in
    /// its closure, settle the anchors whose outputs are already whole, fail the
    /// dependents of a deterministic failure, name the pending anchors it reaches
    /// for the evaluation, promote the closure.
    Eval(EvaluationId),
    /// A wedged evaluation: the `Eval` steps plus the unbacked-output demote.
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
    pub demoted_producers: u64,
    pub cached_reconciled: usize,
    pub adopted: usize,
    pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}

impl ReconcileReport {
    pub fn is_noop(&self) -> bool {
        self.thawed == 0
            && self.demoted_producers == 0
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
pub async fn reconcile_build_graph(ctx: &DbContext, scope: ReconcileScope) -> ReconcileReport {
    let db = &ctx.worker_db;
    let evaluation = scope.evaluation();
    let mut report = ReconcileReport::default();

    match crate::promotion::requeue_failed_closure_for_eval(db, evaluation).await {
        Ok(changes) => {
            report.thawed = changes.len() as u64;
            emit_transition_effects(ctx, &changes).await;
        }
        Err(e) => {
            error!(error = %e, %evaluation, "reconcile: requeue_failed_closure_for_eval failed")
        }
    }

    if let ReconcileScope::Unstick(_) = scope {
        match crate::cache_storage::demote_unbacked_trusted_outputs(ctx).await {
            Ok(n) => report.demoted_producers = n,
            Err(e) => error!(error = %e, "reconcile: demote_unbacked_trusted_outputs failed"),
        }
    }

    // Cache presence is the ground truth for "is this built": anchors whose
    // outputs all exist re-complete even after a requeue, cascade or demote, and
    // the anchors they just made servable advance their dependents' counters.
    match crate::promotion::reconcile_cached_anchors_for_eval(db, evaluation).await {
        Ok(changes) => {
            report.cached_reconciled = changes.len();
            let derivations: Vec<_> = changes.iter().map(|c| c.derivation).collect();
            emit_transition_effects(ctx, &changes).await;
            match crate::readiness::advance_fetchable(db, &derivations).await {
                Ok(advanced) => emit_transition_effects(ctx, &advanced).await,
                Err(e) => error!(error = %e, %evaluation, "reconcile: advance_fetchable failed"),
            }
        }
        Err(e) => {
            error!(error = %e, %evaluation, "reconcile: reconcile_cached_anchors_for_eval failed")
        }
    }

    // Failure-side backstop, paired with the thaw above: fail every non-terminal
    // anchor in the closure reachable from a terminal failure, including the
    // victims the thaw just re-created.
    match crate::promotion::reconcile_dependency_failed(db, evaluation).await {
        Ok(changes) => {
            emit_transition_effects(ctx, &changes).await;
            report.dependency_failed = changes;
        }
        Err(e) => error!(error = %e, "reconcile: reconcile_dependency_failed failed"),
    }

    // A pruned interior in this closure that a thaw or a reset left with no name
    // fails the gate the promote embeds; this evaluation names it first.
    match crate::reachability::adopt_pending_closure(db, evaluation).await {
        Ok(adopted) => {
            report.adopted = adopted.pairs.len();
            // Naming is half of what demand means, so an adoption creates it the way
            // a thaw does.
            for chunk in adopted.derivations().chunks(crate::IN_CHUNK_SIZE) {
                if let Err(e) = crate::readiness::recompute_demand(db, chunk).await {
                    error!(error = %e, %evaluation, "reconcile: demand recompute after adoption failed");
                }
            }
            if let Err(e) = crate::bump_graph_version(db, &adopted.evaluations()).await {
                error!(error = %e, %evaluation, "reconcile: graph version bump after adoption failed");
            }
        }
        Err(e) => error!(error = %e, %evaluation, "reconcile: adopt_pending_closure failed"),
    }

    match crate::readiness::promote_closure(db, evaluation).await {
        Ok(changes) => {
            emit_transition_effects(ctx, &changes).await;
            report.promoted = changes;
        }
        Err(e) => error!(error = %e, "reconcile: promote_closure failed"),
    }

    if !report.is_noop() {
        debug!(
            ?scope,
            thawed = report.thawed,
            demoted = report.demoted_producers,
            cached_reconciled = report.cached_reconciled,
            adopted = report.adopted,
            dependency_failed = report.dependency_failed.len(),
            promoted = report.promoted.len(),
            "graph reconciliation made progress"
        );
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both scopes name an evaluation; there is no tick-driven scope left, so no
    /// caller can ask this for a full-table pass.
    #[test]
    fn every_scope_is_bounded_to_one_evaluation() {
        let eval = EvaluationId::now_v7();
        assert_eq!(ReconcileScope::Eval(eval).evaluation(), eval);
        assert_eq!(ReconcileScope::Unstick(eval).evaluation(), eval);
    }

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
            // the thaw and the cache reconcile move nothing
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
        let report = reconcile_build_graph(&ctx, ReconcileScope::Eval(eval)).await;
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
            .position(|s| s.contains("SET demanded ="))
            .expect("the adoption recomputes what it named");
        assert!(
            adopt < demand && demand < bump && bump < promote,
            "adopt, recompute what a name gave demand to, bump, then promote: {log:?}"
        );
        assert!(
            log[..adopt]
                .iter()
                .any(|s| s.contains("deterministic_blocked")),
            "the thaw runs before the adoption: {log:?}"
        );
    }
}
