/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The tracker is a per-instance cache of candidates and scores. Nothing in it is deciding a
//! hand-out. The claim in Postgres is the arbiter
//! (`gradient_db::scheduling::assignment_record::claim_assignment`).

use std::sync::Arc;
use std::time::Duration;

use gradient_util::supervision::{ChildCtx, ChildSpec};
use tracing::info;

use super::Scheduler;
use crate::job_handlers::ClusterDispatch;
use crate::probe::UpstreamProbe;
use crate::trigger_firing::TriggerDispatch;

mod background;
mod build;
mod eval;
mod runner;

use background::{
    AbandonedDispatchSweep, BuildHistoryRefit, EvalCompletionWatchdog, GraphConsistency,
    GraphStuckReheal, InstanceMetrics, RamShortageSweep, StrandedBuildSweep, TransientRetryRequeue,
    UnreclaimedJobSweep, WorkerLiveness, WorkerSample,
};
use eval::EvalDispatch;
use runner::RunnerDispatch;

#[cfg(test)]
pub(crate) use background::{
    abandoned_assignment_pass, eval_completion_watchdog_pass, liveness_period, stranded_build_pass,
    worker_liveness_pass, worker_sample_pass,
};
pub(crate) use build::BuildKick;
pub(crate) use build::pending_build_jobs;
#[cfg(test)]
pub(crate) use build::{admit_startable_moves, build_assign_pass, resync_startable_set};
#[cfg(test)]
pub(crate) use eval::assign_queued_evals;
pub(crate) use eval::pending_eval_jobs;
pub(crate) use eval::project_id_for_eval;
#[cfg(test)]
pub(crate) use runner::assign_runner_jobs;
pub(crate) use runner::pending_runner_job;

pub(crate) const ASSIGN_TICK_SECS: u64 = 5;
pub(super) const ASSIGN_TICK: Duration = Duration::from_secs(ASSIGN_TICK_SECS);
pub(super) const STARTABLE_RESYNC: Duration = Duration::from_secs(60);
pub(super) const ASSIGN_BUDGET: Duration = Duration::from_secs(120);
const METRICS_BUDGET: Duration = Duration::from_secs(60);
const CONSISTENCY_BUDGET: Duration = Duration::from_secs(600);
const BUILD_HISTORY_REFIT_PERIOD: Duration = Duration::from_secs(300);
const EVAL_WATCHDOG_TICK: Duration = Duration::from_secs(60);

pub fn start_assign_loops(scheduler: Arc<Scheduler>) {
    for spec in child_specs(&scheduler) {
        scheduler.db.shutdown.supervise(spec);
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
    let metrics = &scheduler.db.config.metrics_args;
    let mut children = vec![
        ChildSpec::supervisor(
            "scheduler",
            vec![
                core_child_spec(scheduler),
                ChildSpec::pass(TriggerDispatch {
                    db: scheduler.db.clone(),
                }),
                ChildSpec::pass(EvalDispatch {
                    scheduler: Arc::clone(scheduler),
                }),
                ChildSpec::pass(RunnerDispatch {
                    scheduler: Arc::clone(scheduler),
                }),
                build::child_spec(scheduler),
                ChildSpec::pass(TransientRetryRequeue {
                    graph: Arc::clone(&scheduler.graph),
                }),
                ChildSpec::pass(ClusterDispatch {
                    scheduler: Arc::clone(scheduler),
                }),
                ChildSpec::pass(UpstreamProbe::new(
                    &scheduler.db,
                    &scheduler.graph,
                    &scheduler.upstream_query,
                )),
            ],
        ),
        ChildSpec::pass(WorkerSample {
            scheduler: Arc::clone(scheduler),
            period: Duration::from_secs(metrics.worker_sample_interval_secs.get()),
        }),
        ChildSpec::pass(InstanceMetrics {
            scheduler: Arc::clone(scheduler),
            period: Duration::from_secs(metrics.instance_interval_secs.get()),
        }),
        ChildSpec::pass(BuildHistoryRefit {
            scheduler: Arc::clone(scheduler),
        }),
        ChildSpec::pass(EvalCompletionWatchdog {
            scheduler: Arc::clone(scheduler),
        }),
        ChildSpec::pass(StrandedBuildSweep {
            scheduler: Arc::clone(scheduler),
        }),
        ChildSpec::pass(RamShortageSweep {
            scheduler: Arc::clone(scheduler),
        }),
        ChildSpec::pass(AbandonedDispatchSweep {
            scheduler: Arc::clone(scheduler),
        }),
        ChildSpec::pass(UnreclaimedJobSweep {
            scheduler: Arc::clone(scheduler),
        }),
    ];

    match background::liveness_period(&scheduler.db.config) {
        Some(period) => children.push(ChildSpec::pass(WorkerLiveness {
            scheduler: Arc::clone(scheduler),
            period,
        })),
        None => info!("worker liveness watchdog disabled (worker_heartbeat_timeout_secs = 0)"),
    }

    match metrics.graph_consistency_interval_secs {
        0 => info!("graph consistency check disabled (graph_consistency_interval_secs = 0)"),
        secs => {
            let period = Duration::from_secs(secs);
            children.push(ChildSpec::pass(GraphConsistency {
                db: scheduler.db.clone(),
                period,
            }));
            children.push(ChildSpec::pass(GraphStuckReheal {
                db: scheduler.db.worker_db.clone(),
                graph: Arc::clone(&scheduler.graph),
                period,
            }));
        }
    }

    children
}
