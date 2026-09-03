/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loops that poll the DB and enqueue jobs into the in-memory scheduler.
//!
//! Every loop is a child of one supervision tree (`gradient_util::supervision`):
//! a pass that panics or errors is logged and the loop restarts with backoff, a
//! pass past its budget is cancelled in place, and shutdown stops the tree.
//!
//! Split across submodules by concern:
//! - [`background`] - consistency sweep, worker liveness, and metrics passes
//! - [`eval`] - `dispatch_queued_evals`: finds `Queued` evaluations and enqueues `FlakeJob`s
//! - [`build`] - the build dispatch actor: finds ready `Queued` `derivation_build` anchors and enqueues `BuildJob`s
//!
//! `trigger_dispatch::dispatch_once` fires polling/time triggers and creates evaluations.
//!
//! The eval/build passes are idempotent: re-enqueueing the same job_id overwrites
//! the existing entry in the `JobTracker` without harm.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use gradient_util::supervision::ChildSpec;
use tracing::info;

use super::Scheduler;

mod background;
mod build;
mod eval;

pub(crate) use build::{BuildMsg, dispatch_ready_builds};
#[cfg(test)]
pub(crate) use eval::dispatch_queued_evals;
pub(crate) use eval::project_id_for_eval;

/// Tick interval shared by the eval and build dispatch loops.
pub(crate) const DISPATCH_TICK_SECS: u64 = 5;
pub(super) const DISPATCH_TICK: Duration = Duration::from_secs(DISPATCH_TICK_SECS);
/// A dispatch pass past this is cancelled and retried on the next tick.
pub(super) const DISPATCH_BUDGET: Duration = Duration::from_secs(120);
const METRICS_BUDGET: Duration = Duration::from_secs(60);
const CONSISTENCY_BUDGET: Duration = Duration::from_secs(600);

/// Registers every dispatch loop on the process supervision tree.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    for spec in child_specs(&scheduler) {
        scheduler.state.shutdown.supervise(spec);
    }
}

fn child_specs(scheduler: &Arc<Scheduler>) -> Vec<ChildSpec> {
    let metrics = &scheduler.state.config.metrics_args;
    let mut children = vec![
        periodic(
            scheduler,
            "trigger-dispatch",
            DISPATCH_TICK,
            DISPATCH_BUDGET,
            |s| async move { crate::trigger_dispatch::dispatch_once(&s).await },
        ),
        periodic(
            scheduler,
            "eval-dispatch",
            DISPATCH_TICK,
            DISPATCH_BUDGET,
            |s| async move { eval::dispatch_queued_evals(&s).await },
        ),
        build::child_spec(scheduler),
        periodic(
            scheduler,
            "worker-sample",
            Duration::from_secs(metrics.worker_sample_interval_secs.max(1)),
            METRICS_BUDGET,
            background::worker_sample_pass,
        ),
        periodic(
            scheduler,
            "instance-metrics",
            Duration::from_secs(metrics.instance_metrics_interval_secs.max(1)),
            METRICS_BUDGET,
            background::instance_metrics_pass,
        ),
    ];

    match background::liveness_period(scheduler) {
        Some(period) => children.push(periodic(
            scheduler,
            "worker-liveness",
            period,
            METRICS_BUDGET,
            background::worker_liveness_pass,
        )),
        None => info!("worker liveness watchdog disabled (worker_heartbeat_timeout_secs = 0)"),
    }

    match metrics.graph_consistency_interval_secs {
        0 => info!("graph consistency sweep disabled (graph_consistency_interval_secs = 0)"),
        secs => children.push(periodic(
            scheduler,
            "graph-consistency",
            Duration::from_secs(secs),
            CONSISTENCY_BUDGET,
            background::consistency_sweep_pass,
        )),
    }

    children
}

fn periodic<F, Fut>(
    scheduler: &Arc<Scheduler>,
    name: &'static str,
    period: Duration,
    budget: Duration,
    pass: F,
) -> ChildSpec
where
    F: Fn(Arc<Scheduler>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let scheduler = Arc::clone(scheduler);
    ChildSpec::periodic(name, period, budget, move || {
        let fut = pass(Arc::clone(&scheduler));
        async move { fut.await.map_err(Into::into) }
    })
}
