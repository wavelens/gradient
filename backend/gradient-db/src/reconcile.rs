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
//! runs before `closure_complete` so freshly trusted anchors propagate; the
//! `cached_path`-side fixpoint runs before the anchor-side ones (their gates
//! read cache rows); all flag fixpoints run before promotion and the failure
//! sweep so their gates read sound flags. Each step is logged-and-continued on
//! error - a failing heal must never block the remaining heals. The full
//! derived-flag contract table lives in the `promotion` module doc.

use crate::DbContext;
use crate::status::{TransitionChange, emit_transition_effects};
use gradient_types::EvaluationId;
use tracing::{debug, error};

/// What slice of the graph to heal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReconcileScope {
    /// Every dispatch tick: the anchor-side flag fixpoints (they gate dispatch,
    /// so mid-eval progression rides their cadence) and promotion. Skips the
    /// `cached_path`-side fixpoint, the unbacked-output demote, and the
    /// dependency-failed sweep - those are backstops whose full-table scans
    /// saturated Postgres when re-run every 5s on a large graph.
    Tick,
    /// The periodic backstop (every Nth dispatch tick): global flag fixpoints,
    /// the unbacked-output demote, the dependency-failed sweep, and promotion.
    /// Skips the `cached_path`-side fixpoint - deletion clears those flags
    /// transactionally with a recursive referrer walk
    /// (`clear_gate_flags_for_hashes`) and ingest forward-maintains them, so
    /// re-deriving every row is only the rare `Deep` backstop.
    Global,
    /// `Global` plus the full `cached_path.closure_complete` re-derivation, on
    /// an hourly-order cadence (its CLEAR pass re-verifies every complete
    /// row's whole reference list - tens of seconds on a large cache).
    Deep,
    /// An evaluation just flushed its graph: mark its edges complete, heal
    /// cache-trust across its closure, then the fixpoints and promotion.
    Eval(EvaluationId),
    /// A wedged evaluation (pool can build everything yet nothing dispatches):
    /// like `Eval`, plus thawing terminal-failed anchors across its closure.
    Unstick(EvaluationId),
}

impl ReconcileScope {
    fn evaluation(&self) -> Option<EvaluationId> {
        match self {
            ReconcileScope::Tick | ReconcileScope::Global | ReconcileScope::Deep => None,
            ReconcileScope::Eval(id) | ReconcileScope::Unstick(id) => Some(*id),
        }
    }
}

/// What one reconciliation pass changed. All-zero on a converged graph.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub edges_marked: u64,
    pub thawed: u64,
    pub demoted_producers: u64,
    pub cached_reconciled: usize,
    pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}

impl ReconcileReport {
    pub fn is_noop(&self) -> bool {
        self.edges_marked == 0
            && self.thawed == 0
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
        match crate::promotion::mark_edges_complete_for_eval(db, evaluation).await {
            Ok(n) => report.edges_marked = n,
            Err(e) => {
                error!(error = %e, %evaluation, "reconcile: mark_edges_complete_for_eval failed")
            }
        }
    }

    if let ReconcileScope::Unstick(evaluation) = scope {
        // Thaw terminal-failed anchors anywhere in this eval's closure: a
        // transitive dep a prior eval left failed (and this eval pruned, so it
        // has no build_job here) blocks its dependents with no dispatch to fail
        // and trigger a reactive heal.
        match crate::promotion::requeue_failed_closure_for_eval(db, evaluation).await {
            Ok(n) => report.thawed = n,
            Err(e) => {
                error!(error = %e, %evaluation, "reconcile: requeue_failed_closure_for_eval failed")
            }
        }
    }

    if matches!(
        scope,
        ReconcileScope::Global | ReconcileScope::Deep | ReconcileScope::Eval(_)
    ) {
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

    if matches!(scope, ReconcileScope::Deep | ReconcileScope::Unstick(_))
        && let Err(e) = crate::cache_storage::reconcile_cached_path_closure_complete(db).await
    {
        error!(error = %e, "reconcile: reconcile_cached_path_closure_complete failed");
    }
    if let Err(e) = crate::promotion::reconcile_drv_closure_cached(db).await {
        error!(error = %e, "reconcile: reconcile_drv_closure_cached failed");
    }
    if let Err(e) = crate::promotion::reconcile_closure_complete(db).await {
        error!(error = %e, "reconcile: reconcile_closure_complete failed");
    }

    if matches!(scope, ReconcileScope::Global | ReconcileScope::Deep) {
        // Failure-side backstop: fail every non-terminal anchor reachable from a
        // terminal failure (the reactive cascade misses anchors thawed after
        // their dependency already failed).
        match crate::promotion::reconcile_dependency_failed(db).await {
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
            edges_marked = report.edges_marked,
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
