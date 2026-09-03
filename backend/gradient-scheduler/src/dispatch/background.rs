/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::Scheduler;

/// Poll ~3x per heartbeat deadline so worst-case detection latency is timeout + tick.
const LIVENESS_POLLS_PER_DEADLINE: u64 = 3;

/// Liveness poll period, or `None` when the watchdog is disabled by config.
pub(super) fn liveness_period(scheduler: &Scheduler) -> Option<Duration> {
    let timeout_secs = scheduler.state.config.proto.worker_heartbeat_timeout_secs;
    (timeout_secs != 0)
        .then(|| Duration::from_secs((timeout_secs / LIVENESS_POLLS_PER_DEADLINE).max(5)))
}

/// Read-only invariant check: counts stale gate flags, unpromoted-ready
/// anchors, unbacked trusted outputs, and wedged Building evals so a dead zone
/// becomes a warning long before a user reports a stuck evaluation. Transient
/// non-zero counts right after a transition are normal; persistent ones are not.
pub(super) async fn consistency_sweep_pass(scheduler: Arc<Scheduler>) -> anyhow::Result<()> {
    let report = gradient_db::graph_consistency_report(&scheduler.state.worker_db).await?;
    if report.total() > 0 {
        warn!(
            stale_closure_complete = report.stale_closure_complete,
            stale_drv_closure_cached = report.stale_drv_closure_cached,
            unpromoted_ready = report.unpromoted_ready,
            unbacked_trusted_outputs = report.unbacked_trusted_outputs,
            wedged_building_evals = report.wedged_building_evals,
            "graph consistency sweep found invariant violations"
        );
    } else {
        debug!("graph consistency sweep clean");
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
    let (active_builds, pending_builds) = scheduler.job_tracker.read().await.instance_counts();
    let (total_workers, idle_workers) = scheduler.worker_pool.read().await.worker_counts();
    let counts = crate::instance::InstanceCounts {
        active_builds,
        pending_builds,
        total_workers,
        idle_workers,
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
    let workers = scheduler.worker_pool.read().await.all_workers();
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
