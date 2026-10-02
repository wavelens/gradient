/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::worker;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_db::evaluations::watchdog::LostCompletion;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_graph::Transition;
use gradient_types::ids::DispatchedJobId;
use gradient_types::{DerivationBuildId, EvaluationId};
use gradient_wire::types::BuildFailureKind;
use tracing::{debug, info, warn};

use crate::Scheduler;

const LIVENESS_POLLS_PER_DEADLINE: u64 = 3;

/// The grace must stay comfortably above the graph writer's 600 s RPC timeout. A merely slow
/// transition is then never mistaken for a dropped one.
const LOST_COMPLETION_GRACE_SECS: i64 = 900;

const ABANDONED_ASSIGNMENT_GRACE_SECS: i64 = 1800;

const ABANDONED_ASSIGNMENT_SWEEP_LIMIT: u64 = 10_000;

const ABORT_CONFIRM_GRACE: Duration = Duration::from_secs(300);

pub(super) fn liveness_period(scheduler: &Scheduler) -> Option<Duration> {
    let timeout_secs = scheduler.state.config.proto.worker_heartbeat_timeout_secs;
    (timeout_secs != 0)
        .then(|| Duration::from_secs((timeout_secs / LIVENESS_POLLS_PER_DEADLINE).max(5)))
}

pub(super) async fn consistency_check_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let report =
        gradient_db::graph::consistency::graph_consistency_report(&scheduler.state.db()).await?;
    if report.total() > 0 {
        warn!(
            counter_drift = report.counter_drift,
            walk_drift = report.walk_drift,
            runtime_drift = report.runtime_drift,
            need_drift = report.need_drift,
            skipped_moves = report.skipped_moves,
            unpromoted_startable = report.unpromoted_startable,
            adopted = report.adopted,
            unbacked_trusted_outputs = report.unbacked_trusted_outputs,
            wedged_building_evals = report.wedged_building_evals,
            eval_counter_drift = report.eval_counter_drift,
            scope = report.repair_scope,
            "graph consistency check found invariant violations"
        );
    } else {
        info!(scope = report.repair_scope, "graph consistency check clean");
    }

    Ok(())
}

pub(super) async fn graph_stuck_reheal_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    crate::waiting_state::reheal_graph_stuck_evals(&scheduler.state).await
}

pub(super) async fn worker_liveness_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let timeout_secs = scheduler.state.config.proto.worker_heartbeat_timeout_secs;
    let timeout_ms = (timeout_secs as i64) * 1000;
    let now_ms = gradient_types::now().and_utc().timestamp_millis();
    for worker_id in scheduler.stale_workers(now_ms, timeout_ms).await {
        warn!(
            %worker_id,
            timeout_secs,
            "worker silent past heartbeat deadline - presumed dead (OOM-kill / frozen \
             host / network partition); unregistering and re-queuing its jobs"
        );
        scheduler.unregister_worker(&worker_id).await;
    }
    Ok(())
}

pub(super) async fn instance_metrics_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let c = scheduler.counts().await;
    let counts = crate::instance::InstanceCounts {
        active_builds: c.active_builds,
        pending_builds: c.pending_builds,
        total_workers: c.workers as u32,
        idle_workers: c.idle_workers as u32,
        cpu_core_score_mean: c.cpu_core_score_mean,
    };
    let ctx = crate::instance::compute_instance_context(
        &scheduler.state.worker_db,
        counts,
        gradient_types::now(),
    )
    .await;
    scheduler.instance.store(Arc::new(ctx));

    let eval_history =
        crate::instance::compute_eval_history(&scheduler.state.worker_db, gradient_types::now())
            .await;
    scheduler.eval_history.store(Arc::new(eval_history));
    Ok(())
}

pub(super) async fn worker_sample_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let workers = scheduler.board_workers().await;
    for info in &workers {
        crate::worker_lifecycle::record_worker_sample(&scheduler.state.worker_db, info).await;
    }
    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    scheduler.state.events.publish(worker::QueueDepth {
        workers,
        pending,
        active,
    });
    Ok(())
}

fn plan_abandoned_reap(
    stale: &[(DispatchedJobId, Option<String>)],
    untracked: &HashSet<String>,
) -> Vec<DispatchedJobId> {
    stale
        .iter()
        .filter(|(_, key)| match key {
            Some(key) => untracked.contains(key),
            None => true,
        })
        .map(|(id, _)| *id)
        .collect()
}

pub(super) async fn abandoned_assignment_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    use gradient_entity::dispatched_job::{Column as CDispatchedJob, Entity as EDispatchedJob};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

    scheduler.reap_overdue_aborts(ABORT_CONFIRM_GRACE).await;

    let cutoff = gradient_types::now() - chrono::Duration::seconds(ABANDONED_ASSIGNMENT_GRACE_SECS);
    let stale: Vec<(DispatchedJobId, Option<String>)> = EDispatchedJob::find()
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::DispatchedAt.lt(cutoff))
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
        .limit(ABANDONED_ASSIGNMENT_SWEEP_LIMIT)
        .into_tuple()
        .all(&scheduler.state.worker_db)
        .await?;

    if stale.is_empty() {
        debug!("abandoned dispatch sweep clean");
        return Ok(());
    }

    let untracked: HashSet<String> = scheduler
        .untracked(stale.iter().filter_map(|(_, k)| k.clone()).collect())
        .await
        .into_iter()
        .collect();

    let reap = plan_abandoned_reap(&stale, &untracked);
    if reap.is_empty() {
        return Ok(());
    }

    let reaped = gradient_db::scheduling::assignment_record::abandon_open_assignments(
        &scheduler.state.worker_db,
        &reap,
    )
    .await?;

    warn!(
        rows = reaped,
        grace_secs = ABANDONED_ASSIGNMENT_GRACE_SECS,
        "closed dispatch rows whose job will never report"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvalRepair {
    Complete,
    Fail,
}

/// The tracker, not the database, is deciding what is still in flight. `untracked` is claiming
/// every id during a core outage. A repair pass is then doing nothing instead of racing a returning
/// scheduler.
fn plan_eval_repairs(
    lost: &[LostCompletion],
    untracked: &HashSet<String>,
) -> Vec<(EvaluationId, EvalRepair)> {
    lost.iter()
        .filter(|l| untracked.contains(&crate::jobs::eval_job_key(l.evaluation)))
        .filter_map(|l| {
            let repair = match l.outcome {
                DispatchedJobOutcome::Completed => EvalRepair::Complete,
                DispatchedJobOutcome::Failed => EvalRepair::Fail,
                DispatchedJobOutcome::Abandoned => return None,
            };
            Some((l.evaluation, repair))
        })
        .collect()
}

pub(super) async fn eval_completion_watchdog_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let lost = gradient_db::evaluations::watchdog::lost_eval_completions(
        &scheduler.state.worker_db,
        LOST_COMPLETION_GRACE_SECS,
    )
    .await?;
    if lost.is_empty() {
        debug!("eval completion watchdog clean");
        return Ok(());
    }

    let untracked: HashSet<String> = scheduler
        .untracked(
            lost.iter()
                .map(|l| crate::jobs::eval_job_key(l.evaluation))
                .collect(),
        )
        .await
        .into_iter()
        .collect();

    let mut repaired = 0;
    for (evaluation, repair) in plan_eval_repairs(&lost, &untracked) {
        warn!(
            evaluation_id = %evaluation,
            ?repair,
            grace_secs = LOST_COMPLETION_GRACE_SECS,
            "evaluation stranded past its job's terminal report - re-driving the lost transition"
        );

        let transition = match repair {
            EvalRepair::Complete => Transition::EvalStreamCompleted { evaluation },
            EvalRepair::Fail => Transition::EvalFailed {
                evaluation,
                error: "the worker reported this evaluation failed, but the failure was never \
                        recorded; recovered by the completion watchdog"
                    .to_string(),
                kind: BuildFailureKind::Permanent,
                missing_paths: Vec::new(),
            },
        };

        match scheduler.state.graph.transition(transition).await {
            Ok(_) => repaired += 1,
            Err(e) => {
                warn!(error = %e, evaluation_id = %evaluation, "lost-completion repair failed")
            }
        }
    }

    if repaired > 0 {
        scheduler.kick_assigner();
    }
    Ok(())
}

fn plan_stranded_requeue(
    stranded: &[DerivationBuildId],
    untracked: &HashSet<String>,
) -> Vec<DerivationBuildId> {
    stranded
        .iter()
        .copied()
        .filter(|a| untracked.contains(&crate::jobs::build_job_key(*a)))
        .collect()
}

pub(super) async fn stranded_build_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let stranded = gradient_db::scheduling::build_watchdog::stranded_building_shared_builds(
        &scheduler.state.worker_db,
        LOST_COMPLETION_GRACE_SECS,
    )
    .await?;
    if stranded.is_empty() {
        debug!("stranded build sweep clean");
        return Ok(());
    }

    let untracked: HashSet<String> = scheduler
        .untracked(
            stranded
                .iter()
                .map(|a| crate::jobs::build_job_key(*a))
                .collect(),
        )
        .await
        .into_iter()
        .collect();
    let shared_builds = plan_stranded_requeue(&stranded, &untracked);
    if shared_builds.is_empty() {
        return Ok(());
    }

    warn!(
        shared_builds = shared_builds.len(),
        grace_secs = LOST_COMPLETION_GRACE_SECS,
        "shared builds stranded in Building behind an abandoned dispatch - re-queuing"
    );
    scheduler
        .state
        .graph
        .transition(Transition::OrphanedBuilds { shared_builds })
        .await?;
    scheduler.kick_assigner();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lost(outcome: DispatchedJobOutcome) -> LostCompletion {
        LostCompletion {
            evaluation: EvaluationId::now_v7(),
            outcome,
        }
    }

    fn row(key: Option<&str>) -> (DispatchedJobId, Option<String>) {
        (DispatchedJobId::now_v7(), key.map(str::to_owned))
    }

    #[test]
    fn a_tracked_job_is_never_reaped() {
        let tracked = row(Some("build:still-going"));
        let untracked = HashSet::new();

        assert!(plan_abandoned_reap(&[tracked], &untracked).is_empty());
    }

    #[test]
    fn an_untracked_job_is_reaped() {
        let gone = row(Some("build:worker-vanished"));
        let untracked = HashSet::from(["build:worker-vanished".to_string()]);

        assert_eq!(
            plan_abandoned_reap(std::slice::from_ref(&gone), &untracked),
            vec![gone.0]
        );
    }

    #[test]
    fn a_row_without_a_job_id_is_reaped_on_age_alone() {
        let legacy = row(None);

        assert_eq!(
            plan_abandoned_reap(std::slice::from_ref(&legacy), &HashSet::new()),
            vec![legacy.0]
        );
    }

    #[test]
    fn a_scheduler_outage_reaps_no_tracked_row() {
        let rows = vec![row(Some("build:a")), row(Some("eval:b"))];

        assert!(plan_abandoned_reap(&rows, &HashSet::new()).is_empty());
    }

    #[test]
    fn an_abandoned_job_is_not_re_driven() {
        let rows = vec![lost(DispatchedJobOutcome::Abandoned)];
        let untracked: HashSet<String> = rows
            .iter()
            .map(|l| crate::jobs::eval_job_key(l.evaluation))
            .collect();

        assert!(plan_eval_repairs(&rows, &untracked).is_empty());
    }

    #[test]
    fn a_tracked_stranded_shared_build_is_left_alone() {
        let shared_build = DerivationBuildId::now_v7();

        assert!(plan_stranded_requeue(&[shared_build], &HashSet::new()).is_empty());
    }

    #[test]
    fn an_untracked_stranded_shared_build_is_requeued() {
        let shared_build = DerivationBuildId::now_v7();
        let untracked = HashSet::from([crate::jobs::build_job_key(shared_build)]);

        assert_eq!(
            plan_stranded_requeue(&[shared_build], &untracked),
            vec![shared_build]
        );
    }

    #[test]
    fn each_outcome_maps_to_the_transition_that_was_lost() {
        let rows = vec![
            lost(DispatchedJobOutcome::Completed),
            lost(DispatchedJobOutcome::Failed),
        ];
        let untracked = rows
            .iter()
            .map(|l| crate::jobs::eval_job_key(l.evaluation))
            .collect();

        assert_eq!(
            plan_eval_repairs(&rows, &untracked),
            vec![
                (rows[0].evaluation, EvalRepair::Complete),
                (rows[1].evaluation, EvalRepair::Fail),
            ]
        );
    }

    #[test]
    fn an_evaluation_the_scheduler_still_tracks_is_left_alone() {
        let tracked = lost(DispatchedJobOutcome::Completed);
        let stranded = lost(DispatchedJobOutcome::Completed);
        let untracked = HashSet::from([crate::jobs::eval_job_key(stranded.evaluation)]);

        assert_eq!(
            plan_eval_repairs(&[tracked, stranded], &untracked),
            vec![(stranded.evaluation, EvalRepair::Complete)]
        );
    }

    #[test]
    fn a_core_outage_repairs_nothing() {
        let rows = vec![
            lost(DispatchedJobOutcome::Completed),
            lost(DispatchedJobOutcome::Failed),
        ];
        assert!(plan_eval_repairs(&rows, &HashSet::new()).is_empty());
    }
}
