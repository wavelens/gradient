/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actor;
mod assessment_memo;
pub mod build;
pub mod buildability;
pub mod cluster;
pub mod connection_failures;
pub mod eval;
pub mod history;
pub mod instance;
pub mod jobs;
pub mod log_substitution;
pub mod loops;
pub mod probe;
pub mod views;
pub mod waiting_state;

mod assign_mode;
mod eval_metrics;
mod job_handlers;
pub(crate) mod trigger_firing;
mod unbuildable;
mod worker_lifecycle;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use gradient_core::ServerState;
use gradient_types::*;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};

use actor::{CALL_TIMEOUT, CoreActor, CoreArgs, Counts, SchedulerMsg};

pub use job_handlers::timeline::ReportedTimeline;
pub use jobs::{AssignDecision, BoardActiveJob, DecisionCandidate, PendingJobInfo};

/// This function is pulling the crate into binaries that reference nothing else from it. A linker
/// is dropping an unmentioned rlib, and with it the `gradient_db::sql!` entries the plan gate is
/// reading.
pub const fn link() {}

#[cfg(test)]
mod assign_tests;
#[cfg(test)]
mod scheduler_tests;

#[derive(Clone)]
pub struct Scheduler {
    pub state: Arc<ServerState>,
    core: Arc<tokio::sync::watch::Sender<Option<ActorRef<SchedulerMsg>>>>,
    pub(crate) build_assigner: Arc<arc_swap::ArcSwapOption<ractor::ActorRef<loops::BuildMsg>>>,
    pub(crate) kick_gen: Arc<AtomicU64>,
    pub(crate) policy: Arc<dyn gradient_pool::score::ScoringPolicy>,
    pub(crate) instance: Arc<arc_swap::ArcSwap<gradient_pool::score::InstanceContext>>,
    pub(crate) eval_history: Arc<arc_swap::ArcSwap<crate::instance::EvalHistory>>,
    pub draining: Arc<AtomicBool>,
    pub(crate) assessments: Arc<std::sync::Mutex<assessment_memo::AssessmentMemo>>,
    pub(crate) cluster_wake: Arc<tokio::sync::Notify>,
    pub(crate) prepared:
        Arc<gradient_util::sync::Mutex<std::collections::HashMap<String, cluster::PreparedMember>>>,
    pub(crate) attempts: Arc<gradient_util::sync::Mutex<cluster::AttemptBook>>,
    pub connection_failures: Arc<connection_failures::ConnectionFailures>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(state: Arc<ServerState>) -> Self {
        let policy = gradient_pool::score::policy_by_name(&state.config.scheduler.scoring_policy);
        Self {
            state,
            core: Arc::new(tokio::sync::watch::channel(None).0),
            build_assigner: Arc::new(arc_swap::ArcSwapOption::empty()),
            kick_gen: Arc::new(AtomicU64::new(0)),
            policy,
            instance: Arc::new(arc_swap::ArcSwap::from_pointee(
                gradient_pool::score::InstanceContext::default(),
            )),
            eval_history: Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::instance::EvalHistory::default(),
            )),
            draining: Arc::new(AtomicBool::new(false)),
            assessments: Arc::default(),
            cluster_wake: Arc::new(tokio::sync::Notify::new()),
            prepared: Arc::default(),
            attempts: Arc::default(),
            connection_failures: Arc::default(),
        }
    }

    pub async fn spawn_core(
        &self,
        parent: Option<ActorCell>,
    ) -> Result<ActorRef<SchedulerMsg>, SpawnErr> {
        let args = CoreArgs {
            policy: Arc::clone(&self.policy),
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
        let mut job_ids = vec![crate::jobs::eval_job_key(eval_id)];
        job_ids.extend(
            shared_build_ids
                .iter()
                .map(|id| crate::jobs::build_job_key(*id)),
        );
        if let Err(e) = self
            .call(|reply| SchedulerMsg::RemoveJobs { job_ids, reply })
            .await
        {
            tracing::warn!(error = %e, %eval_id, "cancel_evaluation_jobs did not reach the scheduler");
        }
    }

    pub fn start(self: &Arc<Self>) {
        let scheduler = Arc::downgrade(self);
        self.state.startable_set.on_move(move || {
            if let Some(scheduler) = scheduler.upgrade() {
                scheduler.kick_assigner();
            }
        });
        loops::start_assign_loops(Arc::clone(self));
    }

    pub fn loop_health(&self) -> Vec<(&'static str, gradient_util::supervision::LoopHealth)> {
        self.state
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

        untracked
            .into_iter()
            .filter(|key| self.attempt_of(key).is_none())
            .collect()
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
