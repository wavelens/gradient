/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_id;
use gradient_db::scheduling::assignment_record::{eval_of_key, shared_build_of_key};
use gradient_db::status::{
    EvaluationFailure, update_evaluation_status, update_evaluation_status_with_error,
};
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::{Graph, Transition};
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

async fn eval_assign_count(db: &WorkerDb, evaluation_id: EvaluationId) -> u64 {
    use gradient_entity::dispatched_job::{
        Column as CDispatchedJob, DispatchedJobKind, Entity as EDispatchedJob,
    };
    use sea_orm::PaginatorTrait;

    EDispatchedJob::find()
        .filter(CDispatchedJob::EvaluationId.eq(evaluation_id))
        .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
        .count(db)
        .await
        .unwrap_or_else(|e| {
            warn!(%evaluation_id, error = %e, "eval dispatch count failed; treating as first attempt");
            0
        })
}

async fn abandon_dispatched_jobs(db: &WorkerDb, keys: &[String]) {
    match gradient_db::scheduling::assignment_record::abandon_open_assignments_for_jobs(db, keys)
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
pub async fn requeue_orphaned_jobs(
    db: &DbContext,
    graph: &Graph,
    orphaned: &[PendingJob],
    observed_peak_ram_mb: &std::collections::HashMap<String, u64>,
    cause: gradient_graph::OrphanCause,
) {
    let keys: Vec<String> = orphaned.iter().map(PendingJob::job_key).collect();
    requeue_orphaned_keys(db, graph, &keys, observed_peak_ram_mb, cause).await;
}

pub(crate) async fn requeue_orphaned_keys(
    db: &DbContext,
    graph: &Graph,
    keys: &[String],
    observed_peak_ram_mb: &std::collections::HashMap<String, u64>,
    cause: gradient_graph::OrphanCause,
) {
    abandon_dispatched_jobs(&db.worker_db, keys).await;
    requeue_closed_keys(db, graph, keys, observed_peak_ram_mb, cause).await;
}

pub(crate) async fn requeue_closed_keys(
    db: &DbContext,
    graph: &Graph,
    keys: &[String],
    observed_peak_ram_mb: &std::collections::HashMap<String, u64>,
    cause: gradient_graph::OrphanCause,
) {
    let shared_builds: Vec<DerivationBuildId> = keys
        .iter()
        .filter_map(|key| shared_build_of_key(key))
        .collect();
    let observed_peak_ram_mb = shared_builds
        .iter()
        .filter_map(|id| {
            let peak = observed_peak_ram_mb.get(&crate::jobs::build_job_key(*id))?;
            Some((*id, *peak))
        })
        .collect();
    if !shared_builds.is_empty()
        && let Err(e) = graph
            .transition(Transition::OrphanedBuilds {
                shared_builds,
                cause,
                observed_peak_ram_mb,
            })
            .await
    {
        warn!(error = %e, "requeue orphaned builds did not reach the graph writer");
    }

    for evaluation_id in keys.iter().filter_map(|key| eval_of_key(key)) {
        match by_id::<EEvaluation, _>(&db.worker_db, evaluation_id).await {
            Ok(Some(eval))
                if matches!(
                    eval.status,
                    EvaluationStatus::Fetching
                        | EvaluationStatus::EvaluatingFlake
                        | EvaluationStatus::EvaluatingDerivation
                ) =>
            {
                let assignments = eval_assign_count(&db.worker_db, eval.id).await;
                match orphaned_eval_outcome(assignments, MAX_EVAL_ASSIGN_ATTEMPTS) {
                    OrphanedEval::Requeue => park_orphaned_eval(db, eval).await,
                    OrphanedEval::Exhausted => {
                        warn!(
                            evaluation_id = %eval.id,
                            dispatches = assignments,
                            "evaluation orphaned on every dispatch; failing instead of re-queuing"
                        );
                        if let Err(e) = update_evaluation_status_with_error(
                            db,
                            eval,
                            EvaluationStatus::Failed,
                            &EvaluationFailure::OrphanedOnEveryDispatch { assignments },
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

pub(crate) async fn park_orphaned_eval(db: &DbContext, eval: MEvaluation) {
    persist_waiting_reason(
        &db.worker_db,
        eval.id,
        &eval.waiting_reason,
        Some(&WaitingReason::eval_workers(EvalCapability::Eval, 0)),
    )
    .await;
    let evaluation_id = eval.id;
    if let Err(e) = update_evaluation_status(db, eval, EvaluationStatus::Waiting).await {
        warn!(error = %e, %evaluation_id, "failed to park the orphaned evaluation");
    }
}

pub(crate) async fn requeue_cluster_members(db: &DbContext, graph: &Graph, jobs: &[PendingJob]) {
    let shared_builds: Vec<DerivationBuildId> = jobs
        .iter()
        .filter_map(PendingJob::derivation_build)
        .collect();
    if !shared_builds.is_empty()
        && let Err(e) = graph
            .transition(Transition::OrphanedBuilds {
                shared_builds,
                cause: gradient_graph::OrphanCause::ClusterAttemptEnded,
                observed_peak_ram_mb: Default::default(),
            })
            .await
    {
        warn!(error = %e, "requeue of cluster member builds did not reach the graph writer");
    }
    db.memory
        .startable_set
        .enter(jobs.iter().filter_map(|j| match j {
            PendingJob::Build(b) => Some(b.derivation),
            PendingJob::Eval(_) | PendingJob::Runner(_) => None,
        }));

    for job in jobs.iter().filter(|j| j.derivation_build().is_none()) {
        match by_id::<EEvaluation, _>(&db.worker_db, job.evaluation_id()).await {
            Ok(Some(eval))
                if matches!(
                    eval.status,
                    EvaluationStatus::Fetching
                        | EvaluationStatus::EvaluatingFlake
                        | EvaluationStatus::EvaluatingDerivation
                ) =>
            {
                park_orphaned_eval(db, eval).await
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
