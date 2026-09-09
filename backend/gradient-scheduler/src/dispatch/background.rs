/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_db::LostCompletion;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_graph::Transition;
use gradient_types::EvaluationId;
use gradient_types::proto::BuildFailureKind;
use tracing::{debug, warn};

use crate::Scheduler;

/// Poll ~3x per heartbeat deadline so worst-case detection latency is timeout + tick.
const LIVENESS_POLLS_PER_DEADLINE: u64 = 3;

/// How long an evaluation must sit in the evaluating pair after its job closed
/// before the watchdog calls the terminal report lost. Comfortably above the
/// graph actor's 600 s RPC timeout, so a transition that is merely slow is
/// never mistaken for a dropped one.
const LOST_COMPLETION_GRACE_SECS: i64 = 900;

/// How long a dispatch row may sit open with no job behind it before the reaper
/// closes it. Well above the heartbeat deadline so a worker that is merely slow
/// to check in is never reaped out from under a running job.
const ABANDONED_DISPATCH_GRACE_SECS: i64 = 1800;

/// Liveness poll period, or `None` when the watchdog is disabled by config.
pub(super) fn liveness_period(scheduler: &Scheduler) -> Option<Duration> {
    let timeout_secs = scheduler.state.config.proto.worker_heartbeat_timeout_secs;
    (timeout_secs != 0)
        .then(|| Duration::from_secs((timeout_secs / LIVENESS_POLLS_PER_DEADLINE).max(5)))
}

/// Invariant check: counts stale gate flags, unpromoted-ready anchors, unbacked
/// trusted outputs, wedged Building evals and reference counters driven below
/// zero, so a dead zone becomes a warning long before a user reports a stuck
/// evaluation, and repairs the NAR reference counter over the paths the pending
/// anchors gate on. Transient non-zero counts right after a transition are normal;
/// persistent ones are not - except `nar_counter_drift`, which counts rows this
/// pass already repaired, so the warning can be a successful self-repair. `gating`
/// is the size of the repair's scope, logged either way because it is what the
/// pass costs.
pub(super) async fn consistency_sweep_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let report = gradient_db::graph_consistency_report(&scheduler.state.worker_db).await?;
    if report.total() > 0 {
        warn!(
            stale_closure_complete = report.stale_closure_complete,
            stale_drv_closure_cached = report.stale_drv_closure_cached,
            unpromoted_ready = report.unpromoted_ready,
            unbacked_trusted_outputs = report.unbacked_trusted_outputs,
            wedged_building_evals = report.wedged_building_evals,
            nar_counter_drift = report.nar_counter_drift,
            negative_reference_counters = report.negative_reference_counters,
            gating = report.gating_paths,
            "graph consistency sweep found invariant violations"
        );
    } else {
        debug!(
            gating = report.gating_paths,
            "graph consistency sweep clean"
        );
    }
    Ok(())
}

/// Unregister workers that have gone silent past the heartbeat deadline.
///
/// A worker heartbeats every 10 s; the server otherwise learns of a departure
/// only when the TCP connection closes. A hard OOM-kill, a frozen host, or a
/// network partition can leave the socket half-open with no clean close, so the
/// worker stays "connected" and its in-flight eval/build jobs sit non-terminal
/// forever. This pass reads each worker's `last_seen` (stamped in the session
/// loop) and reuses [`Scheduler::unregister_worker`] - which re-queues the
/// orphaned jobs and resets their DB rows - the moment a worker exceeds the deadline.
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

/// Recompute the windowed [`gradient_score::InstanceContext`] snapshot consumed
/// by resource-aware scoring and publish it lock-free.
pub(super) async fn instance_metrics_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let c = scheduler.counts().await;
    let counts = crate::instance::InstanceCounts {
        active_builds: c.active_builds,
        pending_builds: c.pending_builds,
        total_workers: c.workers as u32,
        idle_workers: c.idle_workers as u32,
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

/// Snapshot every connected worker's live metrics into `worker_sample` for the
/// Job Board's worker statistics.
pub(super) async fn worker_sample_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let workers = scheduler.board_workers().await;
    for info in &workers {
        crate::worker_lifecycle::record_worker_sample(&scheduler.state.worker_db, info).await;
    }
    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    let _ = scheduler
        .state
        .board_events
        .send(crate::BoardEvent::QueueDepth {
            workers,
            pending,
            active,
        });
    Ok(())
}

/// Which stale rows may be closed: those whose job the tracker no longer holds,
/// plus rows written before `job_id` existed, which cannot be matched against the
/// tracker at all and are historical by construction.
///
/// A row the scheduler still tracks is never reaped, however old, so a
/// legitimately long build keeps its open row.
fn plan_abandoned_reap(
    stale: &[(uuid::Uuid, Option<String>)],
    untracked: &HashSet<String>,
) -> Vec<uuid::Uuid> {
    stale
        .iter()
        .filter(|(_, key)| match key {
            Some(key) => untracked.contains(key),
            None => true,
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Close dispatch rows left open by a job that will never report.
///
/// [`crate::build::requeue_orphaned_jobs`] covers a clean disconnect, but a
/// server restart drops the tracker without an unregister for the jobs the old
/// process held, so nothing ever closes their rows and the job board shows them
/// running forever - often on a worker that has since left the fleet.
///
/// The tracker, not the clock, decides what is live: a row whose `job_id` the
/// scheduler still knows is left alone however old it is, so a legitimately long
/// build is never reaped. Rows predating the `job_id` column cannot be matched
/// that way; they are historical by construction and are closed on age alone.
pub(super) async fn abandoned_dispatch_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    use gradient_entity::dispatched_job::{Column as CDispatchedJob, Entity as EDispatchedJob};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, sea_query::Expr};

    let cutoff = gradient_types::now() - chrono::Duration::seconds(ABANDONED_DISPATCH_GRACE_SECS);
    let stale: Vec<(uuid::Uuid, Option<String>)> = EDispatchedJob::find()
        .filter(CDispatchedJob::FinishedAt.is_null())
        .filter(CDispatchedJob::DispatchedAt.lt(cutoff))
        .select_only()
        .column(CDispatchedJob::Id)
        .column(CDispatchedJob::JobId)
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

    let reaped = reap.len();
    EDispatchedJob::update_many()
        .col_expr(
            CDispatchedJob::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(
            CDispatchedJob::Outcome,
            Expr::value(i16::from(DispatchedJobOutcome::Abandoned)),
        )
        .filter(CDispatchedJob::Id.is_in(reap))
        .filter(CDispatchedJob::FinishedAt.is_null())
        .exec(&scheduler.state.worker_db)
        .await?;

    warn!(
        rows = reaped,
        grace_secs = ABANDONED_DISPATCH_GRACE_SECS,
        "closed dispatch rows whose job will never report"
    );
    Ok(())
}

/// Which transition a lost terminal report needs re-sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvalRepair {
    Complete,
    Fail,
}

/// Keep only the evaluations the scheduler has no job for, and map each
/// reported outcome onto the transition that was lost.
///
/// The tracker, not the database, decides what is still in flight: a job the
/// scheduler still holds is one it may yet finish on its own, and `untracked`
/// deliberately claims every id during a core outage so a repair pass does
/// nothing rather than racing a scheduler that is coming back.
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
                // Nothing was reported, so there is no terminal transition to
                // re-send; the orphan path re-queues the evaluation instead.
                DispatchedJobOutcome::Abandoned => return None,
            };
            Some((l.evaluation, repair))
        })
        .collect()
}

/// Re-send the terminal transition for evaluations whose worker report was
/// dropped, so a single lost message stops being permanent.
///
/// `EvalStreamCompleted` is idempotent - it settles edges, reconciles the
/// eval's closure, promotes out of the evaluating pair and finalizes - so
/// re-driving one that did land costs a reconcile and changes nothing.
pub(super) async fn eval_completion_watchdog_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let lost =
        gradient_db::lost_eval_completions(&scheduler.state.worker_db, LOST_COMPLETION_GRACE_SECS)
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
        scheduler.kick_dispatch();
    }
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

    fn row(key: Option<&str>) -> (uuid::Uuid, Option<String>) {
        (uuid::Uuid::now_v7(), key.map(str::to_owned))
    }

    // The whole point of the sweep: a row the tracker still holds is a running
    // job, however long it has been running.
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

    // Rows written before the job_id column cannot be matched against the
    // tracker; they predate the deploy, so age alone settles them.
    #[test]
    fn a_row_without_a_job_id_is_reaped_on_age_alone() {
        let legacy = row(None);

        assert_eq!(
            plan_abandoned_reap(std::slice::from_ref(&legacy), &HashSet::new()),
            vec![legacy.0]
        );
    }

    // `untracked` returns nothing while the scheduler core is down, which must
    // read as "everything is still tracked", not "reap the fleet".
    #[test]
    fn a_scheduler_outage_reaps_no_keyed_row() {
        let rows = vec![row(Some("build:a")), row(Some("eval:b"))];

        assert!(plan_abandoned_reap(&rows, &HashSet::new()).is_empty());
    }

    // An abandoned job never reported, so there is no terminal transition to
    // re-send; re-driving one would invent an outcome the worker never gave.
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
    fn the_outcome_numbers_are_pinned() {
        assert_eq!(i16::from(DispatchedJobOutcome::Completed), 0);
        assert_eq!(i16::from(DispatchedJobOutcome::Failed), 1);
        assert_eq!(i16::from(DispatchedJobOutcome::Abandoned), 2);
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

    /// A job the scheduler still tracks may yet report on its own; re-driving
    /// it would race the handler that is about to run.
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

    /// `untracked` reports every id as tracked while the core is down, which
    /// must read as "repair nothing", not "repair everything".
    #[test]
    fn a_core_outage_repairs_nothing() {
        let rows = vec![
            lost(DispatchedJobOutcome::Completed),
            lost(DispatchedJobOutcome::Failed),
        ];
        assert!(plan_eval_repairs(&rows, &HashSet::new()).is_empty());
    }
}
