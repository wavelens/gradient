/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::Scheduler;

/// Poll ~3x per heartbeat deadline so worst-case detection latency is timeout + tick.
const LIVENESS_POLLS_PER_DEADLINE: u64 = 3;

/// Periodic read-only invariant check: counts stale gate flags, unpromoted-ready
/// anchors, unbacked trusted outputs, and wedged Building evals so a dead zone
/// becomes a warning long before a user reports a stuck evaluation. Transient
/// non-zero counts right after a transition are normal; persistent ones are not.
pub(super) async fn consistency_sweep_loop(scheduler: Arc<Scheduler>) {
    let secs = scheduler
        .state
        .config
        .metrics_args
        .graph_consistency_interval_secs;
    if secs == 0 {
        info!("graph consistency sweep disabled (graph_consistency_interval_secs = 0)");
        return;
    }

    let mut interval = tokio::time::interval(Duration::from_secs(secs.max(1)));
    let cancel = scheduler.state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {}
        }
        match gradient_db::graph_consistency_report(&scheduler.state.worker_db).await {
            Ok(report) if report.total() > 0 => warn!(
                stale_closure_complete = report.stale_closure_complete,
                stale_drv_closure_cached = report.stale_drv_closure_cached,
                unpromoted_ready = report.unpromoted_ready,
                unbacked_trusted_outputs = report.unbacked_trusted_outputs,
                wedged_building_evals = report.wedged_building_evals,
                "graph consistency sweep found invariant violations"
            ),
            Ok(_) => debug!("graph consistency sweep clean"),
            Err(e) => error!(error = %e, "graph consistency sweep failed"),
        }
    }
}

/// Unregister workers that have gone silent past the heartbeat deadline.
///
/// A worker heartbeats every 10 s; the server otherwise learns of a departure
/// only when the TCP connection closes. A hard OOM-kill, a frozen host, or a
/// network partition can leave the socket half-open with no clean close, so the
/// worker stays "connected" and its in-flight eval/build jobs sit non-terminal
/// forever. This watchdog stamps each worker's `last_seen` (in the session loop)
/// and reuses [`Scheduler::unregister_worker`] - which re-queues the orphaned
/// jobs and resets their DB rows - the moment a worker exceeds the deadline.
pub(super) async fn worker_liveness_loop(scheduler: Arc<Scheduler>) {
    let timeout_secs = scheduler.state.config.proto.worker_heartbeat_timeout_secs;
    if timeout_secs == 0 {
        info!("worker liveness watchdog disabled (worker_heartbeat_timeout_secs = 0)");
        return;
    }

    let timeout_ms = (timeout_secs as i64) * 1000;
    // Poll ~3x per deadline so worst-case detection latency is timeout + tick.
    let tick = (timeout_secs / LIVENESS_POLLS_PER_DEADLINE).max(5);
    let mut interval = tokio::time::interval(Duration::from_secs(tick));
    let cancel = scheduler.state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {}
        }
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
    }
}

/// Periodically recompute the windowed [`gradient_score::InstanceContext`] snapshot
/// consumed by resource-aware scoring and publish it lock-free.
pub(super) async fn instance_metrics_loop(scheduler: Arc<Scheduler>) {
    let secs = scheduler
        .state
        .config
        .metrics_args
        .instance_metrics_interval_secs
        .max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    let cancel = scheduler.state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {}
        }
        let (active_builds, pending_builds) =
            scheduler.job_tracker.read().await.instance_counts();
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
            crate::instance::compute_eval_history(&scheduler.state.worker_db, gradient_types::now()).await;
        scheduler.eval_history.store(Arc::new(eval_history));
    }
}

/// Periodically snapshot every connected worker's live metrics into
/// `worker_sample` for the Job Board's worker statistics.
pub(super) async fn worker_sample_loop(scheduler: Arc<Scheduler>) {
    let secs = scheduler
        .state
        .config
        .metrics_args
        .worker_sample_interval_secs
        .max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    let cancel = scheduler.state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {}
        }
        let workers = scheduler.worker_pool.read().await.all_workers();
        for info in &workers {
            crate::worker_lifecycle::record_worker_sample(&scheduler.state.worker_db, info).await;
        }
        let (workers, pending, active) = scheduler.metrics_snapshot().await;
        let _ = scheduler
            .state
            .board_events
            .send(crate::BoardEvent::QueueDepth { workers, pending, active });
    }
}
