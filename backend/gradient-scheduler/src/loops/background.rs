/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::worker;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_db::evaluations::watchdog::LostCompletion;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_graph::{Graph, RequeueScope, Transition};
use gradient_types::ids::DispatchedJobId;
use gradient_types::{DerivationBuildId, EvaluationId, RuntimeConfig};
use gradient_util::supervision::{Pass, PassError, RunsOn};
use gradient_wire::types::BuildFailureKind;
use tracing::{debug, info, warn};

use crate::Scheduler;
use crate::count::clamp_to_u32;

const LIVENESS_POLLS_PER_DEADLINE: u64 = 3;

/// The grace must stay comfortably above the graph writer's 600 s RPC timeout. A merely slow
/// transition is then never mistaken for a dropped one.
const LOST_COMPLETION_GRACE_SECS: i64 = 900;

const ABANDONED_ASSIGNMENT_GRACE_SECS: i64 = 1800;

const ABANDONED_ASSIGNMENT_SWEEP_LIMIT: u64 = 10_000;

const ABORT_CONFIRM_GRACE: Duration = Duration::from_secs(300);

pub(crate) fn liveness_period(config: &RuntimeConfig) -> Option<Duration> {
    let timeout_secs = config.proto.worker_heartbeat_timeout_secs;
    (timeout_secs != 0)
        .then(|| Duration::from_secs((timeout_secs / LIVENESS_POLLS_PER_DEADLINE).max(5)))
}

pub(crate) struct GraphConsistency {
    pub(crate) db: DbContext,
    pub(crate) period: Duration,
}

impl Pass for GraphConsistency {
    const NAME: &'static str = "graph-consistency";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        consistency_check_pass(&self.db).await.map_err(Into::into)
    }
}

async fn consistency_check_pass(db: &DbContext) -> anyhow::Result<()> {
    let report = gradient_db::graph::consistency::graph_consistency_report(db).await?;
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

pub(crate) struct TransientRetryRequeue {
    pub(crate) graph: Arc<Graph>,
}

impl Pass for TransientRetryRequeue {
    const NAME: &'static str = "transient-retry-requeue";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::ASSIGN_BUDGET;

    fn period(&self) -> Duration {
        super::ASSIGN_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        transient_retry_pass(&self.graph).await.map_err(Into::into)
    }
}

async fn transient_retry_pass(graph: &Graph) -> anyhow::Result<()> {
    graph.requeue(RequeueScope::TransientRetries).await?;
    Ok(())
}

pub(crate) struct GraphStuckReheal {
    pub(crate) db: WorkerDb,
    pub(crate) graph: Arc<Graph>,
    pub(crate) period: Duration,
}

impl Pass for GraphStuckReheal {
    const NAME: &'static str = "graph-stuck-reheal";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        crate::waiting_state::reheal_graph_stuck_evals(&self.db, &self.graph)
            .await
            .map_err(Into::into)
    }
}

pub(crate) struct WorkerLiveness {
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) period: Duration,
}

impl Pass for WorkerLiveness {
    const NAME: &'static str = "worker-liveness";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::METRICS_BUDGET;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        worker_liveness_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) async fn worker_liveness_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let timeout_secs = scheduler.db.config.proto.worker_heartbeat_timeout_secs;
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
    scheduler.requeue_unreclaimed(Instant::now()).await;
    Ok(())
}

pub(crate) struct InstanceMetrics {
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) period: Duration,
}

impl Pass for InstanceMetrics {
    const NAME: &'static str = "instance-metrics";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::METRICS_BUDGET;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        instance_metrics_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) struct BuildHistoryRefit {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for BuildHistoryRefit {
    const NAME: &'static str = "build-history-refit";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        super::BUILD_HISTORY_REFIT_PERIOD
    }

    async fn run(&self) -> Result<(), PassError> {
        let history = &self.scheduler.scoring.build_history;
        history
            .refresh(&self.scheduler.db.worker_db)
            .await
            .map_err(anyhow::Error::from)?;
        history.refit(self.scheduler.db.config.server.retention_days);
        Ok(())
    }
}

async fn instance_metrics_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let c = scheduler.counts().await;
    let counts = crate::instance::InstanceCounts {
        active_builds: c.active_builds,
        pending_builds: c.pending_builds,
        total_workers: clamp_to_u32(c.workers),
        idle_workers: clamp_to_u32(c.idle_workers),
        cpu_core_score_mean: c.cpu_core_score_mean,
        upload_speed_mean_mbps: c.upload_speed_mean_mbps,
        download_speed_mean_mbps: c.download_speed_mean_mbps,
        download_slots: u32::try_from(scheduler.db.config.nar.max_concurrent_downloads.get())
            .unwrap_or(u32::MAX),
        upload_slots: u32::try_from(scheduler.db.config.upload.concurrency.get())
            .unwrap_or(u32::MAX),
    };
    let ctx = crate::instance::compute_instance_context(
        &scheduler.db.worker_db,
        counts,
        gradient_types::now(),
    )
    .await;
    let fleet_core_score = ctx.cpu_core_score_mean;
    scheduler.scoring.instance.store(Arc::new(ctx));

    let eval_history = crate::instance::compute_eval_history(
        &scheduler.db.worker_db,
        gradient_types::now(),
        fleet_core_score,
    )
    .await;
    scheduler.scoring.eval_history.store(Arc::new(eval_history));
    Ok(())
}

pub(crate) struct WorkerSample {
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) period: Duration,
}

impl Pass for WorkerSample {
    const NAME: &'static str = "worker-sample";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::METRICS_BUDGET;

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        worker_sample_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) async fn worker_sample_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let workers = scheduler.board_workers().await;
    for info in &workers {
        crate::worker_lifecycle::record_worker_sample(&scheduler.db.worker_db, info).await;
    }
    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    scheduler.db.events.publish(worker::QueueDepth {
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

pub(crate) struct AbandonedDispatchSweep {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for AbandonedDispatchSweep {
    const NAME: &'static str = "abandoned-dispatch-sweep";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        super::EVAL_WATCHDOG_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        abandoned_assignment_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) async fn abandoned_assignment_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
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
        .all(&scheduler.db.worker_db)
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
        &scheduler.db.worker_db,
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
        .filter(|l| untracked.contains(&crate::jobs::eval_job_key(l.evaluation, 0)))
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

pub(crate) struct EvalCompletionWatchdog {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for EvalCompletionWatchdog {
    const NAME: &'static str = "eval-completion-watchdog";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        super::EVAL_WATCHDOG_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        eval_completion_watchdog_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) async fn eval_completion_watchdog_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let lost = gradient_db::evaluations::watchdog::lost_eval_completions(
        &scheduler.db.worker_db,
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
                .map(|l| crate::jobs::eval_job_key(l.evaluation, 0))
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

        let repaired_one = match repair {
            EvalRepair::Complete => scheduler.complete_eval_stream(evaluation).await,
            EvalRepair::Fail => scheduler
                .graph
                .transition(Transition::EvalFailed {
                    evaluation,
                    error: "the worker reported this evaluation failed, but the failure was never \
                        recorded; recovered by the completion watchdog"
                        .to_string(),
                    kind: BuildFailureKind::Permanent,
                    missing_paths: Vec::new(),
                })
                .await
                .map(|_| ()),
        };

        match repaired_one {
            Ok(()) => repaired += 1,
            Err(e) => {
                warn!(error = %e, evaluation_id = %evaluation, "lost-completion repair failed")
            }
        }
    }

    if repaired > 0 {
        scheduler.build_kick.kick();
    }
    Ok(())
}

pub(crate) struct RamShortageSweep {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for RamShortageSweep {
    const NAME: &'static str = "ram-shortage-sweep";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        super::EVAL_WATCHDOG_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        ram_shortage_pass(&self.scheduler).await.map_err(Into::into)
    }
}

async fn ram_shortage_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.is_draining().await {
        return Ok(());
    }

    let shortages = scheduler.lasting_ram_shortages().await;
    if shortages.is_empty() {
        return Ok(());
    }

    warn!(
        shared_builds = shortages.len(),
        grace_secs = crate::ram_shortage::RAM_SHORTAGE_GRACE.as_secs(),
        "queued builds need more RAM than any connected worker has free - failing them"
    );
    scheduler
        .graph
        .transition(Transition::NotEnoughResources { shortages })
        .await?;
    scheduler.build_kick.kick();
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

pub(crate) struct StrandedBuildSweep {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for StrandedBuildSweep {
    const NAME: &'static str = "stranded-build-sweep";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = super::CONSISTENCY_BUDGET;

    fn period(&self) -> Duration {
        super::EVAL_WATCHDOG_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        stranded_build_pass(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

pub(crate) async fn stranded_build_pass(scheduler: &Scheduler) -> anyhow::Result<()> {
    let stranded = gradient_db::scheduling::build_watchdog::stranded_building_shared_builds(
        &scheduler.db.worker_db,
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
        .graph
        .transition(Transition::OrphanedBuilds {
            shared_builds,
            cause: gradient_graph::OrphanCause::WorkerLost,
            observed_peak_ram_mb: Default::default(),
        })
        .await?;
    scheduler.build_kick.kick();
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
    fn abandoned_rows_are_reaped_only_when_untracked() {
        let gone = row(Some("build:worker-vanished"));
        let untracked = HashSet::from(["build:worker-vanished".to_string()]);
        assert_eq!(
            plan_abandoned_reap(std::slice::from_ref(&gone), &untracked),
            vec![gone.0]
        );

        let legacy = row(None);
        assert_eq!(
            plan_abandoned_reap(std::slice::from_ref(&legacy), &HashSet::new()),
            vec![legacy.0]
        );
    }

    #[test]
    fn tracked_rows_are_kept_through_a_scheduler_outage() {
        let tracked = row(Some("build:still-going"));
        assert!(plan_abandoned_reap(&[tracked], &HashSet::new()).is_empty());

        let rows = vec![row(Some("build:a")), row(Some("eval:b"))];
        assert!(plan_abandoned_reap(&rows, &HashSet::new()).is_empty());
    }

    #[test]
    fn stranded_shared_builds_are_requeued_only_when_untracked() {
        let shared_build = DerivationBuildId::now_v7();
        assert!(plan_stranded_requeue(&[shared_build], &HashSet::new()).is_empty());

        let untracked = HashSet::from([crate::jobs::build_job_key(shared_build)]);
        assert_eq!(
            plan_stranded_requeue(&[shared_build], &untracked),
            vec![shared_build]
        );
    }

    #[test]
    fn eval_repairs_map_each_outcome_to_the_lost_transition() {
        let rows = vec![
            lost(DispatchedJobOutcome::Completed),
            lost(DispatchedJobOutcome::Failed),
        ];
        let untracked = rows
            .iter()
            .map(|l| crate::jobs::eval_job_key(l.evaluation, 0))
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
    fn eval_repairs_skip_tracked_abandoned_and_core_outage_rows() {
        let tracked = lost(DispatchedJobOutcome::Completed);
        let stranded = lost(DispatchedJobOutcome::Completed);
        let untracked = HashSet::from([crate::jobs::eval_job_key(stranded.evaluation, 0)]);
        assert_eq!(
            plan_eval_repairs(&[tracked, stranded], &untracked),
            vec![(stranded.evaluation, EvalRepair::Complete)]
        );

        let abandoned = vec![lost(DispatchedJobOutcome::Abandoned)];
        let untracked: HashSet<String> = abandoned
            .iter()
            .map(|l| crate::jobs::eval_job_key(l.evaluation, 0))
            .collect();
        assert!(plan_eval_repairs(&abandoned, &untracked).is_empty());

        let outage = vec![
            lost(DispatchedJobOutcome::Completed),
            lost(DispatchedJobOutcome::Failed),
        ];
        assert!(plan_eval_repairs(&outage, &HashSet::new()).is_empty());
    }
}
