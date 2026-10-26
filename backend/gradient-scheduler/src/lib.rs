/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actor;
mod assessment_memo;
pub mod build;
pub mod build_history;
pub mod buildability;
pub mod cluster;
pub mod connection_failures;
pub mod draining;
pub mod eval;
pub mod eval_split;
pub mod import_waits;
pub mod input_waits;
pub mod instance;
pub mod jobs;
pub mod log_substitution;
pub mod loops;
pub mod probe;
pub mod reclaim;
pub mod views;
pub mod waiting_state;

mod assign_mode;
mod count;
mod eval_metrics;
mod job_handlers;
mod ram_shortage;
mod realisations;
mod scoring;
mod supersede;
pub mod trigger_firing;
mod unbuildable;
mod worker_lifecycle;
mod worker_memory;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::maintenance::recovery::KeptAssignment;
use gradient_graph::Graph;
use gradient_types::*;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::Semaphore;

use actor::{CALL_TIMEOUT, CoreActor, CoreArgs, Counts, SchedulerMsg};
use cluster::ClusterAttempts;
use loops::BuildKick;
use scoring::Scoring;

pub use job_handlers::timeline::ReportedTimeline;
pub use jobs::{AssignDecision, BoardActiveJob, DecisionCandidate, PendingJobInfo};

/// Linkers drop an unmentioned rlib and with it the `gradient_db::sql!` entries of the plan check.
pub const fn link() {}

#[cfg(test)]
mod cases;
#[cfg(test)]
mod test_jobs;

#[derive(Clone)]
pub struct Scheduler {
    pub db: DbContext,
    pub graph: Arc<Graph>,
    pub started_at: DateTime<Utc>,
    pub(crate) upstream_query: Arc<Semaphore>,
    core: Arc<tokio::sync::watch::Sender<Option<ActorRef<SchedulerMsg>>>>,
    pub cluster: ClusterAttempts,
    pub(crate) scoring: Scoring,
    pub(crate) build_kick: BuildKick,
    pub draining: draining::Draining,
    pub connection_failures: Arc<connection_failures::ConnectionFailures>,
    pub(crate) import_waits: import_waits::ImportWaits,
    pub(crate) input_waits: input_waits::InputWaits,
    pub(crate) workers: worker_memory::WorkerMemory,
    pub(crate) split_decisions: eval_split::create::SplitDecisions,
    expected_reclaims: Arc<gradient_util::sync::Mutex<Vec<KeptAssignment>>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(state: Arc<ServerState>) -> Self {
        let policy = gradient_pool::score::policy_for(state.config.scheduler.scoring_policy);
        Self {
            db: state.db(),
            graph: Arc::clone(&state.graph),
            started_at: state.started_at,
            upstream_query: Arc::clone(&state.memory.upstream_query),
            core: Arc::new(tokio::sync::watch::channel(None).0),
            cluster: ClusterAttempts::default(),
            scoring: Scoring::new(policy),
            build_kick: BuildKick::default(),
            draining: draining::Draining::default(),
            connection_failures: Arc::default(),
            import_waits: import_waits::ImportWaits::default(),
            input_waits: input_waits::InputWaits::default(),
            workers: worker_memory::WorkerMemory::default(),
            split_decisions: Default::default(),
            expected_reclaims: Arc::default(),
        }
    }

    pub async fn spawn_core(
        &self,
        parent: Option<ActorCell>,
    ) -> Result<ActorRef<SchedulerMsg>, SpawnErr> {
        let args = CoreArgs {
            policy: Arc::clone(&self.scoring.policy),
            kept: self.held_for_new_core().await,
            reclaim_grace: std::time::Duration::from_secs(
                self.db.config.proto.assignment_reclaim_grace_secs,
            ),
        };
        let (actor, _) = match parent {
            Some(parent) => Actor::spawn_linked(None, CoreActor, args, parent).await?,
            None => Actor::spawn(None, CoreActor, args).await?,
        };
        self.core.send_replace(Some(actor.clone()));
        Ok(actor)
    }

    pub fn core_changes(&self) -> tokio::sync::watch::Receiver<Option<ActorRef<SchedulerMsg>>> {
        self.core.subscribe()
    }

    async fn core(&self) -> anyhow::Result<ActorRef<SchedulerMsg>> {
        let mut rx = self.core.subscribe();
        let live = tokio::time::timeout(CALL_TIMEOUT, rx.wait_for(|c| c.is_some()))
            .await
            .map_err(|_| anyhow::anyhow!("scheduler core unavailable"))?
            .map_err(|_| anyhow::anyhow!("scheduler core closed"))?;
        Ok(live.clone().expect("wait_for guarantees Some"))
    }

    pub(crate) async fn call<T: Send + 'static>(
        &self,
        msg: impl FnOnce(RpcReplyPort<T>) -> SchedulerMsg,
    ) -> anyhow::Result<T> {
        use ractor::rpc::CallResult;
        match self.core().await?.call(msg, Some(CALL_TIMEOUT)).await {
            Ok(CallResult::Success(v)) => Ok(v),
            Ok(CallResult::Timeout) => Err(anyhow::anyhow!("scheduler call timed out")),
            Ok(CallResult::SenderError) => Err(anyhow::anyhow!("scheduler core dropped the reply")),
            Err(e) => Err(anyhow::anyhow!("scheduler core unreachable: {e}")),
        }
    }

    pub(crate) async fn cast(&self, msg: SchedulerMsg) -> anyhow::Result<()> {
        self.core()
            .await?
            .send_message(msg)
            .map_err(|e| anyhow::anyhow!("scheduler core unreachable: {e}"))
    }

    pub async fn cancel_evaluation_jobs(
        &self,
        eval_id: EvaluationId,
        shared_build_ids: &[DerivationBuildId],
    ) {
        let split_members = u16::try_from(eval_split::create::MAX_SPLIT_MEMBERS).unwrap_or(1);
        let mut job_ids: Vec<String> = (0..split_members)
            .map(|index| crate::jobs::eval_job_key(eval_id, index))
            .collect();
        job_ids.extend(
            shared_build_ids
                .iter()
                .map(|id| crate::jobs::build_job_key(*id)),
        );
        self.cancel_jobs(eval_id, job_ids).await;
    }

    pub(crate) async fn cancel_jobs(&self, eval_id: EvaluationId, job_ids: Vec<String>) {
        match self
            .call(|reply| SchedulerMsg::CancelJobs { job_ids, reply })
            .await
        {
            Ok(running) => {
                for (worker_id, job_id) in running {
                    tracing::info!(%worker_id, %job_id, %eval_id, "sent AbortJob to worker");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, %eval_id, "cancelling jobs did not reach the scheduler");
            }
        }
    }

    pub fn start(self: &Arc<Self>) {
        let scheduler = Arc::downgrade(self);
        self.db.memory.startable_set.on_move(move || {
            if let Some(scheduler) = scheduler.upgrade() {
                scheduler.build_kick.kick();
            }
        });
        loops::start_assign_loops(Arc::clone(self));
    }

    pub fn loop_health(&self) -> Vec<(&'static str, gradient_util::supervision::LoopHealth)> {
        self.db
            .shutdown
            .supervision_health()
            .map(|h| h.snapshot())
            .unwrap_or_default()
    }

    pub async fn counts(&self) -> Counts {
        self.call(|reply| SchedulerMsg::Counts { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn metrics_snapshot(&self) -> (usize, usize, usize) {
        let c = self.counts().await;
        (c.workers, c.pending, c.active)
    }

    pub async fn pending_jobs_snapshot(&self) -> Vec<jobs::PendingJobInfo> {
        self.call(|reply| SchedulerMsg::PendingSnapshot { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn board_active_jobs(&self) -> Vec<jobs::BoardActiveJob> {
        self.call(|reply| SchedulerMsg::BoardActiveJobs { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn recent_decisions(&self) -> Vec<jobs::AssignDecision> {
        self.call(|reply| SchedulerMsg::RecentDecisions { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn candidate_detail(
        &self,
        id: gradient_types::ids::DispatchedJobId,
    ) -> Option<jobs::CandidateDetail> {
        self.call(|reply| SchedulerMsg::CandidateDetail { id, reply })
            .await
            .ok()
            .flatten()
    }

    pub async fn active_job(&self, job_id: &str) -> Option<jobs::PendingJob> {
        let job_id = job_id.to_owned();
        self.call(|reply| SchedulerMsg::ActiveJob { job_id, reply })
            .await
            .ok()
            .flatten()
    }

    pub async fn owns_dispatch(
        &self,
        worker: &str,
        job_id: &str,
        assignment_id: gradient_types::ids::DispatchedJobId,
    ) -> bool {
        let (worker, job_id) = (worker.to_owned(), job_id.to_owned());
        self.call(|reply| SchedulerMsg::OwnsDispatch {
            worker,
            job_id,
            assignment_id,
            reply,
        })
        .await
        .unwrap_or(false)
    }

    pub async fn pending_job(&self, job_id: &str) -> Option<jobs::PendingJob> {
        let job_id = job_id.to_owned();
        self.call(|reply| SchedulerMsg::PendingJob { job_id, reply })
            .await
            .ok()
            .flatten()
    }

    /// A core outage is counting every id as tracked. A dispatch pass is then enqueuing nothing
    /// instead of duplicating work. A member whose report is waiting for its attempt's verdict is
    /// in neither map but still owned.
    pub async fn untracked(&self, job_ids: Vec<String>) -> Vec<String> {
        let untracked = self
            .call(|reply| SchedulerMsg::Untracked { job_ids, reply })
            .await
            .unwrap_or_default();

        let mut unowned = Vec::with_capacity(untracked.len());
        for key in untracked {
            if self.cluster.book.attempt_of(&key).await.is_none() {
                unowned.push(key);
            }
        }

        unowned
    }

    pub(crate) async fn lasting_ram_shortages(&self) -> Vec<gradient_graph::RamShortage> {
        self.call(|reply| SchedulerMsg::LastingRamShortages { reply })
            .await
            .unwrap_or_default()
    }

    pub(crate) async fn prune_pending_builds(
        &self,
        stale: impl Fn(&jobs::PendingBuildJob) -> bool + Send + 'static,
    ) -> usize {
        let stale = Box::new(stale);
        self.call(|reply| SchedulerMsg::PrunePendingBuilds { stale, reply })
            .await
            .unwrap_or_default()
    }
}
