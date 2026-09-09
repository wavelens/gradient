/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one graph reconciler. Every self-heal of the build graph runs through
//! [`reconcile_build_graph`] with a [`ReconcileScope`]; the canonical step
//! ordering lives here and nowhere else. Historically the "reconcile then
//! promote" pipeline was hand-copied at three call sites (dispatch tick, eval
//! completion, graph-unstick) with divergent step subsets and orderings, and
//! every new dead-zone fix meant patching some subset of the three - this is
//! the single place such a fix lives now.
//!
//! Ordering rationale: demotes run before the flag fixpoints so a flag cleared
//! by a demote is re-propagated in the same pass; the cached-anchor reconcile
//! runs before `closure_complete` so freshly trusted anchors propagate; all
//! flag fixpoints run before promotion and the failure sweep so their gates
//! read sound flags. Each step is logged-and-continued on error - a failing
//! heal must never block the remaining heals. The full derived-flag contract
//! table lives in the `promotion` module doc.

use crate::DbContext;
use crate::status::{TransitionChange, emit_transition_effects};
use gradient_types::EvaluationId;
use tracing::{debug, error};

/// What slice of the graph to heal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReconcileScope {
    /// Every dispatch tick: promotion only. Skips the anchor-side flag
    /// fixpoints, the unbacked-output demote and the dependency-failed sweep -
    /// all full-table scans that saturated Postgres when re-run every 5s on a
    /// large graph. Mid-eval progression is carried by reactive
    /// `propagate_closure_complete` (per completion) and the per-flush `Eval`
    /// passes; the global fixpoint backstop rides `Global`.
    Tick,
    /// The periodic backstop (every Nth dispatch tick): global flag fixpoints,
    /// the unbacked-output demote, the dependency-failed sweep, and promotion.
    Global,
    /// An evaluation just flushed its graph: thaw the terminal-failed anchors
    /// in its closure, heal cache-trust across it, then the fixpoints and
    /// promotion.
    Eval(EvaluationId),
    /// A wedged evaluation (pool can build everything yet nothing dispatches):
    /// the `Eval` steps again, run on demand.
    Unstick(EvaluationId),
}

impl ReconcileScope {
    fn evaluation(&self) -> Option<EvaluationId> {
        match self {
            ReconcileScope::Tick | ReconcileScope::Global => None,
            ReconcileScope::Eval(id) | ReconcileScope::Unstick(id) => Some(*id),
        }
    }

    /// Whether this pass runs the anchor-side flag fixpoints
    /// (`drv_closure_cached`, `closure_complete`). Every scope EXCEPT the plain
    /// 5s `Tick` does: `Eval`/`Unstick` heal only their eval's closure (bounded),
    /// while `Global` (30s) runs the global full-table backstop. `Tick` skips
    /// them - on a large converged graph the full-table CLEAR/SET is seconds
    /// long, and re-running it every 5s saturated Postgres and starved the
    /// dispatch loop it shares. Mid-eval progression does not need it: the
    /// reactive `propagate_closure_complete` marks `closure_complete` on every
    /// completion, and the per-flush `Eval`-scoped passes converge an eval's
    /// `drv_closure_cached` while it evaluates.
    fn runs_anchor_fixpoints(&self) -> bool {
        !matches!(self, ReconcileScope::Tick)
    }
}

/// What one reconciliation pass changed. All-zero on a converged graph.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub thawed: u64,
    pub demoted_producers: u64,
    pub cached_reconciled: usize,
    pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}

impl ReconcileReport {
    pub fn is_noop(&self) -> bool {
        self.thawed == 0
            && self.demoted_producers == 0
            && self.cached_reconciled == 0
            && self.dependency_failed.is_empty()
            && self.promoted.is_empty()
    }
}

/// Run the canonical healing pipeline for `scope`. Effects (dep-count deltas,
/// board events, CI checks, eval finalization) fan out through the one emitter
/// for every anchor a step moved, so a reconciliation can never move an anchor
/// without its consequences.
pub async fn reconcile_build_graph(ctx: &DbContext, scope: ReconcileScope) -> ReconcileReport {
    let db = &ctx.worker_db;
    let mut report = ReconcileReport::default();

    if let Some(evaluation) = scope.evaluation() {
        // A prior evaluation's terminal failure is not this one's verdict:
        // thaw across the closure once, now that the closure is known.
        match crate::promotion::requeue_failed_closure_for_eval(db, evaluation).await {
            Ok(n) => report.thawed = n,
            Err(e) => {
                error!(error = %e, %evaluation, "reconcile: requeue_failed_closure_for_eval failed")
            }
        }
    }

    if matches!(scope, ReconcileScope::Global | ReconcileScope::Eval(_)) {
        // Heal the cache-trust invariant before the fixpoints and promotion: a
        // trusted producer whose output artifact is gone (GC, partial cache
        // hit) is demoted to a fresh build intent so dependents stop failing
        // InputsUnavailable.
        match crate::cache_storage::demote_unbacked_trusted_outputs(db, &ctx.storage.nar_storage)
            .await
        {
            Ok(n) => report.demoted_producers = n,
            Err(e) => error!(error = %e, "reconcile: demote_unbacked_trusted_outputs failed"),
        }
    }

    if let Some(evaluation) = scope.evaluation() {
        // Cache presence is the ground truth for "is this built": anchors whose
        // outputs all exist re-complete even after a requeue/cascade/demote reset.
        match crate::promotion::reconcile_cached_anchors_for_eval(db, evaluation).await {
            Ok(changes) => {
                report.cached_reconciled = changes.len();
                emit_transition_effects(ctx, &changes).await;
            }
            Err(e) => {
                error!(error = %e, %evaluation, "reconcile: reconcile_cached_anchors_for_eval failed")
            }
        }
    }

    // Anchor-side flag fixpoints. A per-eval scope (`Eval`/`Unstick`) bounds them
    // to that eval's closure; `Global` runs the global full-table backstop.
    // The plain 5s `Tick` skips them entirely - the full-table scan is seconds
    // long on a large graph and saturated the dispatch loop's DB when re-run
    // every 5s, while reactive `propagate_closure_complete` and the per-flush
    // `Eval` passes already carry mid-eval progression.
    if scope.runs_anchor_fixpoints() {
        let fixpoint_scope = scope.evaluation();
        if let Err(e) = crate::promotion::reconcile_drv_closure_cached(db, fixpoint_scope).await {
            error!(error = %e, "reconcile: reconcile_drv_closure_cached failed");
        }
        if let Err(e) = crate::promotion::reconcile_closure_complete(db, fixpoint_scope).await {
            error!(error = %e, "reconcile: reconcile_closure_complete failed");
        }
    }

    if scope.runs_anchor_fixpoints() {
        // Failure-side backstop, paired with the requeue thaw above: fail every
        // non-terminal anchor reachable from a terminal failure (the reactive
        // cascade misses anchors thawed after their dependency already failed).
        // `Eval`/`Unstick` bound it to the eval closure so the graph-stuck heal
        // re-fails its own thawed victims inline; `Global` sweeps the whole
        // table. `Tick` skips it, as with the other anchor fixpoints.
        match crate::promotion::reconcile_dependency_failed(db, scope.evaluation()).await {
            Ok(changes) => {
                emit_transition_effects(ctx, &changes).await;
                report.dependency_failed = changes;
            }
            Err(e) => error!(error = %e, "reconcile: reconcile_dependency_failed failed"),
        }
    }

    // Success-side backstop: promote every Created anchor whose deps are all
    // satisfied (leaves, already-cached deps, missed completion windows).
    match crate::promotion::promote_ready(db).await {
        Ok(changes) => {
            emit_transition_effects(ctx, &changes).await;
            report.promoted = changes;
        }
        Err(e) => error!(error = %e, "reconcile: promote_ready failed"),
    }

    if !report.is_noop() {
        debug!(
            ?scope,
            thawed = report.thawed,
            demoted = report.demoted_producers,
            cached_reconciled = report.cached_reconciled,
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

    /// The anchor-side fixpoints skip the plain 5s `Tick` (its full-table scan
    /// saturated the shared dispatch DB); `Global` runs the global backstop
    /// (`evaluation()` is `None`) and `Eval`/`Unstick` run a closure-bounded
    /// pass (`evaluation()` is `Some`).
    #[test]
    fn anchor_fixpoints_skip_tick_and_bound_to_eval_when_scoped() {
        let eval = EvaluationId::now_v7();
        assert!(!ReconcileScope::Tick.runs_anchor_fixpoints());
        assert!(ReconcileScope::Global.runs_anchor_fixpoints());
        assert!(ReconcileScope::Eval(eval).runs_anchor_fixpoints());
        assert!(ReconcileScope::Unstick(eval).runs_anchor_fixpoints());

        assert!(ReconcileScope::Global.evaluation().is_none());
        assert!(ReconcileScope::Eval(eval).evaluation().is_some());
        assert!(ReconcileScope::Unstick(eval).evaluation().is_some());
    }
}
