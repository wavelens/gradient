/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job scheduler - tracks connected workers and dispatches eval/build jobs.
//!
//! Injected into the axum router as an `Extension<Arc<Scheduler>>`.
//!
//! The `Scheduler` impl is split across submodules by concern:
//! - [`worker_lifecycle`] - connect / disconnect / capability updates
//! - [`job_handlers`] - queue, assignment, status, completion, log, abort
//! - [`dispatch`] - background loops that poll the DB and enqueue jobs
//! - [`build`] - `BuildOutput`/completion/failure handling and self-heal
//! - [`waiting_state`] - reconciles evaluation status against the worker pool
//! - [`buildability`] - whether the connected pool can build a pending anchor

pub mod actor;
pub mod build;
pub mod buildability;
pub mod dispatch;
pub mod eval;
pub mod history;
pub mod instance;
pub mod jobs;
pub mod log_substitution;
pub mod peer_auth;
pub mod views;
pub mod waiting_state;
pub mod worker_pool;
pub mod worker_state;

mod dispatch_mode;
mod eval_metrics;
mod job_handlers;
pub(crate) mod trigger_dispatch;
mod worker_lifecycle;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use gradient_core::ServerState;
use gradient_types::*;
use ractor::{Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::RwLock;

use actor::{CALL_TIMEOUT, CoreActor, CoreArgs, Counts, SchedulerMsg};

/// Per-evaluation accumulator of discovered `(drv_path, dependencies)` pairs.
/// Fully-resolvable pairs flush to `derivation_dependency` edges after each
/// batch (`eval::flush_ready_edges`, so builds dispatch mid-stream); the
/// remainder flushes at stream completion. Entries drop when the eval
/// completes, fails, or is aborted.
type EvalEdgesMap = Arc<RwLock<HashMap<EvaluationId, eval::EvalEdgeAccumulator>>>;

pub use gradient_types::BoardEvent;
pub use jobs::{BoardActiveJob, DecisionCandidate, DispatchDecision, PendingJobInfo};
pub use worker_pool::WorkerInfo;

#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod scheduler_tests;

/// The shared scheduler - clone freely (all fields are `Arc`s).
#[derive(Clone)]
pub struct Scheduler {
    /// Shared application state (DB, CLI config, etc.).
    pub state: Arc<ServerState>,
    /// The live state actor, republished on every (re)spawn; callers wait on
    /// the watch so a restart looks like latency, not an error.
    core: Arc<tokio::sync::watch::Sender<Option<ActorRef<SchedulerMsg>>>>,
    /// The live build-dispatch actor, re-published by its factory on every
    /// (re)spawn, so `kick_dispatch` always reaches the current instance.
    pub(crate) build_dispatch: Arc<arc_swap::ArcSwapOption<ractor::ActorRef<dispatch::BuildMsg>>>,
    /// Edge-trigger generation for `kick_dispatch`: the dispatcher services a
    /// burst of kicks with one pass by comparing against the generation it saw.
    pub(crate) kick_gen: Arc<AtomicU64>,
    /// Per-evaluation accumulator of discovered dependency edges, flushed when
    /// the eval stream completes. Promotion itself is graph-driven (see
    /// `gradient_db::promotion`), not tied to this map.
    pub(crate) eval_edges: EvalEdgesMap,
    /// Scoring policy used when selecting which pending job to assign to a
    /// requesting worker.  Shared via `Arc` so it can be read lock-free.
    pub(crate) policy: Arc<dyn gradient_score::ScoringPolicy>,
    /// Windowed instance metrics snapshot, recomputed periodically by
    /// `instance_metrics_loop` and read lock-free during scoring.
    pub(crate) instance: Arc<arc_swap::ArcSwap<gradient_score::InstanceContext>>,
    /// Per-task eval-RAM prediction (p95 peak RSS), refreshed by
    /// `instance_metrics_loop`, consumed by eval scoring.
    pub(crate) eval_history: Arc<
        arc_swap::ArcSwap<
            std::collections::HashMap<
                gradient_types::ids::TaskId,
                gradient_score::HistoryPrediction,
            >,
        >,
    >,
    /// Instance draining toggle (superuser): when set, dispatch is paused and
    /// in-flight evaluations are parked so the server can be stopped safely.
    /// In-memory only, so it auto-clears on the next startup.
    pub draining: Arc<AtomicBool>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(state: Arc<ServerState>) -> Self {
        let policy = gradient_score::policy_by_name(&state.config.eval.scheduler_scoring_policy);
        Self {
            state,
            core: Arc::new(tokio::sync::watch::channel(None).0),
            build_dispatch: Arc::new(arc_swap::ArcSwapOption::empty()),
            kick_gen: Arc::new(AtomicU64::new(0)),
            eval_edges: Arc::new(RwLock::new(HashMap::new())),
            policy,
            instance: Arc::new(arc_swap::ArcSwap::from_pointee(
                gradient_score::InstanceContext::default(),
            )),
            eval_history: Arc::new(arc_swap::ArcSwap::from_pointee(
                std::collections::HashMap::new(),
            )),
            draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawn the state actor, linked to `parent` when supervised, and publish it.
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

    /// Observe core (re)publications; the sessions supervisor re-registers on each.
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

    /// Drop the eval job and its build jobs from the tracker. Workers already
    /// assigned finish or time out; the DB-side abort is the caller's job.
    pub async fn cancel_evaluation_jobs(
        &self,
        eval_id: EvaluationId,
        anchor_ids: &[DerivationBuildId],
    ) {
        let mut job_ids = vec![format!("eval:{eval_id}")];
        job_ids.extend(anchor_ids.iter().map(|id| format!("build:{id}")));
        if let Err(e) = self.call(|reply| SchedulerMsg::RemoveJobs { job_ids, reply }).await {
            tracing::warn!(error = %e, %eval_id, "cancel_evaluation_jobs did not reach the scheduler");
        }
        self.eval_edges.write().await.remove(&eval_id);
    }

    /// Spawn background task polling, eval dispatch, and build dispatch loops.
    ///
    /// Call once after creating the scheduler, before serving requests.
    pub fn start(self: &Arc<Self>) {
        dispatch::start_dispatch_loops(Arc::clone(self));
    }

    /// Per-loop supervision health (restarts, pass errors, timeouts, last ok).
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

    /// `(workers_connected, jobs_pending, jobs_active)` for the metrics endpoint.
    pub async fn metrics_snapshot(&self) -> (usize, usize, usize) {
        let c = self.counts().await;
        (c.workers, c.pending, c.active)
    }

    pub async fn pending_jobs_snapshot(&self) -> Vec<jobs::PendingJobInfo> {
        self.call(|reply| SchedulerMsg::PendingSnapshot { reply })
            .await
            .unwrap_or_default()
    }

    /// Per-dimension classification of in-flight jobs for the worker-load radar.
    pub async fn board_active_jobs(&self) -> Vec<jobs::BoardActiveJob> {
        self.call(|reply| SchedulerMsg::BoardActiveJobs { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn recent_decisions(&self) -> Vec<jobs::DispatchDecision> {
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

    /// The subset of `job_ids` the tracker knows nothing about (neither pending
    /// nor active). On a core outage every id counts as tracked, so a dispatch
    /// pass enqueues nothing rather than duplicating work.
    pub async fn untracked(&self, job_ids: Vec<String>) -> Vec<String> {
        self.call(|reply| SchedulerMsg::Untracked { job_ids, reply })
            .await
            .unwrap_or_default()
    }
}
