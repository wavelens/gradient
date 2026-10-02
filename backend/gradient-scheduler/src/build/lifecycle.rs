/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_core::ServerState;
use gradient_db::status::{update_evaluation_status, update_evaluation_status_with_error};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{info, warn};

use crate::jobs::PendingJob;
use crate::waiting_state::persist_waiting_reason;

/// A healthy evaluation is spending exactly one dispatch, and each disconnect is costing another.
/// An evaluation killing every worker it touches would be re-dispatched forever without this
/// ceiling.
pub(crate) const MAX_EVAL_ASSIGN_ATTEMPTS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrphanedEval {
    Requeue,
    Exhausted,
}

pub(crate) fn orphaned_eval_outcome(assignments: u64, budget: u64) -> OrphanedEval {
    if assignments >= budget {
        OrphanedEval::Exhausted
    } else {
        OrphanedEval::Requeue
    }
}

async fn eval_assign_count(state: &Arc<ServerState>, evaluation_id: EvaluationId) -> u64 {
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

async fn abandon_dispatched_jobs(state: &Arc<ServerState>, orphaned: &[PendingJob]) {
    let keys: Vec<String> = orphaned.iter().map(PendingJob::job_key).collect();

    match gradient_db::scheduling::assignment_record::abandon_open_assignments_for_jobs(
        &state.worker_db,
        &keys,
    )
    .await
    {
        Ok(rows) if rows > 0 => {
            info!(rows, "closed dispatch telemetry for orphaned jobs");
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "failed to close dispatch telemetry for orphaned jobs"),
    }
}

/// The state machine is letting evaluations reach `Queued` only via `Waiting`. Orphaned evaluations
/// park to `Waiting`, and the repair pass right after is recovering them to `Queued`.
pub async fn requeue_orphaned_jobs(state: &Arc<ServerState>, orphaned: &[PendingJob]) {
    abandon_dispatched_jobs(state, orphaned).await;

    let shared_builds: Vec<DerivationBuildId> = orphaned
        .iter()
        .filter_map(|j| j.derivation_build())
        .collect();
    if !shared_builds.is_empty()
        && let Err(e) = state
            .graph
            .transition(Transition::OrphanedBuilds { shared_builds })
            .await
    {
        warn!(error = %e, "requeue orphaned builds did not reach the graph writer");
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
                let assignments = eval_assign_count(state, eval.id).await;
                match orphaned_eval_outcome(assignments, MAX_EVAL_ASSIGN_ATTEMPTS) {
                    OrphanedEval::Requeue => park_orphaned_eval(state, eval).await,
                    OrphanedEval::Exhausted => {
                        warn!(
                            evaluation_id = %eval.id,
                            dispatches = assignments,
                            "evaluation orphaned on every dispatch; failing instead of re-queuing"
                        );
                        if let Err(e) = update_evaluation_status_with_error(
                            &state.db(),
                            eval,
                            EvaluationStatus::Failed,
                            format!(
                                "evaluation was dispatched {assignments} times and each worker \
                                 disconnected before reporting a result; giving up"
                            ),
                            Some("scheduler".to_string()),
                        )
                        .await
                        {
                            warn!(error = %e, %evaluation_id, "failed to fail the orphaned evaluation");
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, %evaluation_id, "requeue orphaned eval: load failed"),
        }
    }
}

pub(crate) async fn park_orphaned_eval(state: &Arc<ServerState>, eval: MEvaluation) {
    persist_waiting_reason(
        state,
        eval.id,
        &eval.waiting_reason,
        Some(&WaitingReason::eval_workers(EvalCapability::Eval, 0)),
    )
    .await;
    let evaluation_id = eval.id;
    if let Err(e) = update_evaluation_status(&state.db(), eval, EvaluationStatus::Waiting).await {
        warn!(error = %e, %evaluation_id, "failed to park the orphaned evaluation");
    }
}

pub(crate) async fn requeue_cluster_members(state: &Arc<ServerState>, jobs: &[PendingJob]) {
    let shared_builds: Vec<DerivationBuildId> = jobs
        .iter()
        .filter_map(PendingJob::derivation_build)
        .collect();
    if !shared_builds.is_empty()
        && let Err(e) = state
            .graph
            .transition(Transition::OrphanedBuilds { shared_builds })
            .await
    {
        warn!(error = %e, "requeue of cluster member builds did not reach the graph writer");
    }
    state
        .startable_set
        .enter(jobs.iter().filter_map(|j| match j {
            PendingJob::Build(b) => Some(b.derivation),
            PendingJob::Eval(_) => None,
        }));

    for job in jobs.iter().filter(|j| j.derivation_build().is_none()) {
        match EEvaluation::find_by_id(job.evaluation_id())
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
                park_orphaned_eval(state, eval).await
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, evaluation_id = %job.evaluation_id(), "requeue cluster member eval: load failed")
            }
        }
    }
}

#[cfg(test)]
mod orphaned_eval_tests {
    use super::{MAX_EVAL_ASSIGN_ATTEMPTS, OrphanedEval, orphaned_eval_outcome};

    #[test]
    fn an_eval_under_budget_is_requeued() {
        for assignments in 1..MAX_EVAL_ASSIGN_ATTEMPTS {
            assert_eq!(
                orphaned_eval_outcome(assignments, MAX_EVAL_ASSIGN_ATTEMPTS),
                OrphanedEval::Requeue,
                "dispatch {assignments} of {MAX_EVAL_ASSIGN_ATTEMPTS}"
            );
        }
    }

    #[test]
    fn an_eval_that_spends_its_budget_stops_being_requeued() {
        assert_eq!(
            orphaned_eval_outcome(MAX_EVAL_ASSIGN_ATTEMPTS, MAX_EVAL_ASSIGN_ATTEMPTS),
            OrphanedEval::Exhausted
        );
        assert_eq!(
            orphaned_eval_outcome(MAX_EVAL_ASSIGN_ATTEMPTS + 20, MAX_EVAL_ASSIGN_ATTEMPTS),
            OrphanedEval::Exhausted
        );
    }
}
