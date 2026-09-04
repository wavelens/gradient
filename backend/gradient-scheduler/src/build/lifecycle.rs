/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-queue of the jobs a worker disconnect orphaned, and the eval dispatch budget.

use std::sync::Arc;

use gradient_core::ServerState;
use gradient_db::{update_evaluation_status, update_evaluation_status_with_error};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::warn;

use crate::jobs::PendingJob;
use crate::waiting_state::persist_waiting_reason;

/// How many times an evaluation may be handed to a worker before the
/// scheduler stops re-queuing it. A healthy eval spends exactly one; each
/// dispatch that ends in a disconnect rather than a result costs another.
/// Without this ceiling an eval whose worker keeps dying mid-evaluation is
/// re-dispatched forever, taking the fleet's eval capacity with it every
/// round.
pub(crate) const MAX_EVAL_DISPATCH_ATTEMPTS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrphanedEval {
    /// Budget remains: park the eval so a worker can pick it up again.
    Requeue,
    /// Budget spent: fail the eval instead of looping.
    Exhausted,
}

/// Decide what to do with an evaluation orphaned by a worker disconnect,
/// given how many times it has already been dispatched (this dispatch
/// included).
pub(crate) fn orphaned_eval_outcome(dispatches: u64, budget: u64) -> OrphanedEval {
    if dispatches >= budget {
        OrphanedEval::Exhausted
    } else {
        OrphanedEval::Requeue
    }
}

/// How many times this evaluation has been handed to a worker, from the
/// dispatch telemetry. A load failure counts as zero so a DB hiccup can never
/// fail an otherwise healthy evaluation.
async fn eval_dispatch_count(state: &Arc<ServerState>, evaluation_id: EvaluationId) -> u64 {
    use gradient_entity::dispatched_job::{
        Column as CDispatchedJob, DispatchedJobKind, Entity as EDispatchedJob,
    };
    use sea_orm::PaginatorTrait;

    EDispatchedJob::find()
        .filter(CDispatchedJob::EvaluationId.eq(evaluation_id))
        .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
        .count(&state.worker_db)
        .await
        .unwrap_or_else(|e| {
            warn!(%evaluation_id, error = %e, "eval dispatch count failed; treating as first attempt");
            0
        })
}

/// Re-queue the in-flight jobs orphaned by a worker disconnect so they
/// re-dispatch instead of lingering in a non-terminal DB status. Anchors move
/// `Building -> Queued`; evaluations (which the state machine only lets reach
/// `Queued` via `Waiting`) park to `Waiting` so the reconciler that runs right
/// after recovers them to `Queued` once an eval-capable worker is free.
pub async fn requeue_orphaned_jobs(state: &Arc<ServerState>, orphaned: &[PendingJob]) {
    let anchors: Vec<DerivationBuildId> = orphaned
        .iter()
        .filter_map(|j| j.derivation_build())
        .collect();
    if !anchors.is_empty()
        && let Err(e) = state
            .graph
            .transition(Transition::OrphanedBuilds { anchors })
            .await
    {
        warn!(error = %e, "requeue orphaned builds did not reach the graph actor");
    }

    for job in orphaned.iter().filter(|j| j.derivation_build().is_none()) {
        let evaluation_id = job.evaluation_id();
        match EEvaluation::find_by_id(evaluation_id)
            .one(&state.worker_db)
            .await
        {
            Ok(Some(eval))
                if matches!(
                    eval.status,
                    EvaluationStatus::Fetching
                        | EvaluationStatus::EvaluatingFlake
                        | EvaluationStatus::EvaluatingDerivation
                ) =>
            {
                let dispatches = eval_dispatch_count(state, eval.id).await;
                match orphaned_eval_outcome(dispatches, MAX_EVAL_DISPATCH_ATTEMPTS) {
                    OrphanedEval::Requeue => {
                        persist_waiting_reason(
                            state,
                            eval.id,
                            &eval.waiting_reason,
                            Some(&WaitingReason::eval_workers(EvalCapability::Eval, 0)),
                        )
                        .await;
                        update_evaluation_status(&state.db(), eval, EvaluationStatus::Waiting)
                            .await;
                    }
                    OrphanedEval::Exhausted => {
                        warn!(
                            evaluation_id = %eval.id,
                            dispatches,
                            "evaluation orphaned on every dispatch; failing instead of re-queuing"
                        );
                        update_evaluation_status_with_error(
                            &state.db(),
                            eval,
                            EvaluationStatus::Failed,
                            format!(
                                "evaluation was dispatched {dispatches} times and each worker \
                                 disconnected before reporting a result; giving up"
                            ),
                            Some("scheduler".to_string()),
                        )
                        .await;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, %evaluation_id, "requeue orphaned eval: load failed"),
        }
    }
}

#[cfg(test)]
mod orphaned_eval_tests {
    use super::{MAX_EVAL_DISPATCH_ATTEMPTS, OrphanedEval, orphaned_eval_outcome};

    /// The common case: a worker drops once, the eval goes back on the queue.
    #[test]
    fn an_eval_under_budget_is_requeued() {
        for dispatches in 1..MAX_EVAL_DISPATCH_ATTEMPTS {
            assert_eq!(
                orphaned_eval_outcome(dispatches, MAX_EVAL_DISPATCH_ATTEMPTS),
                OrphanedEval::Requeue,
                "dispatch {dispatches} of {MAX_EVAL_DISPATCH_ATTEMPTS}"
            );
        }
    }

    /// An eval that wedges every worker it touches must terminate: without
    /// this it is re-dispatched forever, parking to `Waiting` between rounds
    /// and starving the fleet each time it runs.
    #[test]
    fn an_eval_that_spends_its_budget_stops_being_requeued() {
        assert_eq!(
            orphaned_eval_outcome(MAX_EVAL_DISPATCH_ATTEMPTS, MAX_EVAL_DISPATCH_ATTEMPTS),
            OrphanedEval::Exhausted
        );
        assert_eq!(
            orphaned_eval_outcome(MAX_EVAL_DISPATCH_ATTEMPTS + 20, MAX_EVAL_DISPATCH_ATTEMPTS),
            OrphanedEval::Exhausted
        );
    }
}
