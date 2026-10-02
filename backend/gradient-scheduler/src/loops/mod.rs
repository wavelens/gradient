/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The tracker is a per-instance cache of candidates and scores. Nothing in it is deciding a
//! hand-out. The claim in Postgres is the arbiter
//! (`gradient_db::scheduling::assignment_record::claim_assignment`).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use gradient_util::supervision::{ChildCtx, ChildSpec};
use tracing::info;

use super::Scheduler;

mod background;
mod build;
mod eval;

pub(crate) use build::BuildMsg;
#[cfg(test)]
pub(crate) use build::{admit_startable_moves, resync_startable_set};
#[cfg(test)]
pub(crate) use eval::assign_queued_evals;
pub(crate) use eval::project_id_for_eval;

pub(crate) const ASSIGN_TICK_SECS: u64 = 5;
pub(super) const ASSIGN_TICK: Duration = Duration::from_secs(ASSIGN_TICK_SECS);
pub(super) const STARTABLE_RESYNC: Duration = Duration::from_secs(60);
pub(super) const ASSIGN_BUDGET: Duration = Duration::from_secs(120);
const METRICS_BUDGET: Duration = Duration::from_secs(60);
const CONSISTENCY_BUDGET: Duration = Duration::from_secs(600);
const EVAL_WATCHDOG_TICK: Duration = Duration::from_secs(60);

pub fn start_assign_loops(scheduler: Arc<Scheduler>) {
    for spec in child_specs(&scheduler) {
        scheduler.state.shutdown.supervise(spec);
    }
}

fn core_child_spec(scheduler: &Arc<Scheduler>) -> ChildSpec {
    let scheduler = Arc::clone(scheduler);
    ChildSpec::Custom {
        stop_last: false,
        name: "scheduler-core",
        spawn: Arc::new(move |ctx: ChildCtx| {
            let scheduler = Arc::clone(&scheduler);
            Box::pin(async move { Ok(scheduler.spawn_core(Some(ctx.parent)).await?.get_cell()) })
        }),
    }
}

fn child_specs(scheduler: &Arc<Scheduler>) -> Vec<ChildSpec> {
    let metrics = &scheduler.state.config.metrics_args;
    let mut children = vec![
        ChildSpec::supervisor(
            "scheduler",
            vec![
                core_child_spec(scheduler),
                periodic(
                    scheduler,
                    "trigger-dispatch",
                    ASSIGN_TICK,
                    ASSIGN_BUDGET,
                    |s| async move { crate::trigger_firing::fire_once(&s).await },
                ),
                eval_assign_spec(scheduler),
                build::child_spec(scheduler),
                cluster_assign_spec(scheduler),
                crate::probe::child_spec(&scheduler.state),
            ],
        ),
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
            Duration::from_secs(metrics.instance_interval_secs.max(1)),
            METRICS_BUDGET,
            background::instance_metrics_pass,
        ),
        periodic(
            scheduler,
            "eval-completion-watchdog",
            EVAL_WATCHDOG_TICK,
            CONSISTENCY_BUDGET,
            background::eval_completion_watchdog_pass,
        ),
        periodic(
            scheduler,
            "stranded-build-sweep",
            EVAL_WATCHDOG_TICK,
            CONSISTENCY_BUDGET,
            background::stranded_build_pass,
        ),
        periodic(
            scheduler,
            "abandoned-dispatch-sweep",
            EVAL_WATCHDOG_TICK,
            CONSISTENCY_BUDGET,
            background::abandoned_assignment_pass,
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
        0 => info!("graph consistency check disabled (graph_consistency_interval_secs = 0)"),
        secs => {
            children.push(periodic(
                scheduler,
                "graph-consistency",
                Duration::from_secs(secs),
                CONSISTENCY_BUDGET,
                background::consistency_check_pass,
            ));
            children.push(periodic(
                scheduler,
                "graph-stuck-reheal",
                Duration::from_secs(secs),
                CONSISTENCY_BUDGET,
                background::graph_stuck_reheal_pass,
            ));
        }
    }

    children
}

fn cluster_assign_spec(scheduler: &Arc<Scheduler>) -> ChildSpec {
    let wake = Arc::clone(&scheduler.cluster_wake);
    let scheduler = Arc::clone(scheduler);
    ChildSpec::periodic_woken(
        "cluster-dispatch",
        ASSIGN_TICK,
        ASSIGN_BUDGET,
        wake,
        move || {
            let scheduler = Arc::clone(&scheduler);
            async move { scheduler.plan_clusters().await.map_err(Into::into) }
        },
    )
}

fn eval_assign_spec(scheduler: &Arc<Scheduler>) -> ChildSpec {
    let wake = Arc::clone(&scheduler.state.eval_assign_wake);
    let scheduler = Arc::clone(scheduler);
    ChildSpec::periodic_woken(
        "eval-dispatch",
        ASSIGN_TICK,
        ASSIGN_BUDGET,
        wake,
        move || {
            let scheduler = Arc::clone(&scheduler);
            async move {
                eval::assign_queued_evals(&scheduler)
                    .await
                    .map_err(Into::into)
            }
        },
    )
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
