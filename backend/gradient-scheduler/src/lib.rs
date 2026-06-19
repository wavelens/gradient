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

pub mod build;
pub mod dispatch;
pub mod eval;
pub mod history;
pub mod instance;
pub mod jobs;
pub mod log_substitution;
pub mod peer_auth;
pub mod views;
pub mod worker_pool;
pub mod worker_state;

mod dispatch_mode;
mod eval_metrics;
mod job_handlers;
pub(crate) mod trigger_dispatch;
mod worker_lifecycle;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gradient_types::*;
use gradient_core::ServerState;
use tokio::sync::RwLock;

use jobs::JobTracker;
use worker_pool::WorkerPool;

/// Per-evaluation accumulator of discovered `(drv_path, dependencies)` pairs.
/// Flushed to `derivation_dependency` edges once the eval stream completes
/// (every endpoint has a row). Entries drop when the eval completes, fails, or
/// is aborted.
type EvalEdgesMap = Arc<RwLock<HashMap<EvaluationId, Vec<(String, Vec<String>)>>>>;

pub use gradient_types::BoardEvent;
pub use jobs::{DecisionCandidate, DispatchDecision, PendingJobInfo};
pub use worker_pool::WorkerInfo;

#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod handler_tests;
#[cfg(test)]
mod scheduler_tests;

/// The shared scheduler - clone freely (all fields are `Arc`s).
#[derive(Clone)]
pub struct Scheduler {
    /// Shared application state (DB, CLI config, etc.).
    pub state: Arc<ServerState>,
    pub(crate) worker_pool: Arc<RwLock<WorkerPool>>,
    pub(crate) job_tracker: Arc<RwLock<JobTracker>>,
    /// Bumped when new jobs are enqueued so handler dispatch loops can push
    /// `JobOffer` messages to connected workers. A `watch` generation counter
    /// (not `Notify`) so a bump fired while a session is busy is still observed
    /// on its next loop iteration instead of being a lost edge-triggered wakeup.
    pub(crate) job_notify: Arc<tokio::sync::watch::Sender<u64>>,
    /// Kicks `build_dispatch_loop` to run a dispatch pass immediately instead of
    /// waiting for its 5s tick, so a serial dependency chain advances at
    /// completion speed rather than one level per interval. `notify_one` keeps a
    /// single permit, so a kick fired mid-pass is still serviced next iteration.
    pub(crate) dispatch_kick: Arc<tokio::sync::Notify>,
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
    /// Per-project eval-RAM prediction (p95 peak RSS), refreshed by
    /// `instance_metrics_loop`, consumed by eval scoring.
    pub(crate) eval_history:
        Arc<arc_swap::ArcSwap<std::collections::HashMap<gradient_types::ids::ProjectId, gradient_score::HistoryPrediction>>>,
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
            worker_pool: Arc::new(RwLock::new(WorkerPool::new())),
            job_tracker: Arc::new(RwLock::new(JobTracker::new())),
            job_notify: Arc::new(tokio::sync::watch::channel(0u64).0),
            dispatch_kick: Arc::new(tokio::sync::Notify::new()),
            eval_edges: Arc::new(RwLock::new(HashMap::new())),
            policy,
            instance: Arc::new(arc_swap::ArcSwap::from_pointee(gradient_score::InstanceContext::default())),
            eval_history: Arc::new(arc_swap::ArcSwap::from_pointee(std::collections::HashMap::new())),
            draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Drop the eval job and any associated build jobs from the in-memory
    /// tracker. Workers that have already been assigned will finish or time out
    /// normally; the DB-side abort (via `gradient_ci::abort_evaluation`) is the
    /// caller's responsibility.
    pub async fn cancel_evaluation_jobs(&self, eval_id: EvaluationId, build_ids: &[BuildId]) {
        let mut tracker = self.job_tracker.write().await;
        tracker.remove_job(&format!("eval:{eval_id}"));
        for bid in build_ids {
            tracker.remove_job(&format!("build:{bid}"));
        }

        self.eval_edges.write().await.remove(&eval_id);
    }

    /// Spawn background project polling, eval dispatch, and build dispatch loops.
    ///
    /// Call once after creating the scheduler, before serving requests.
    pub fn start(self: &Arc<Self>) {
        dispatch::start_dispatch_loops(Arc::clone(self));
    }

    /// Snapshot of in-memory scheduler counts used by the metrics endpoint.
    /// Returns `(workers_connected, jobs_pending, jobs_active)`.
    pub async fn metrics_snapshot(&self) -> (usize, usize, usize) {
        let workers = self.worker_pool.read().await.worker_count();
        let tracker = self.job_tracker.read().await;
        (workers, tracker.pending_count(), tracker.active_count())
    }

    pub async fn pending_jobs_snapshot(&self) -> Vec<jobs::PendingJobInfo> {
        self.job_tracker.read().await.pending_snapshot()
    }

    pub async fn recent_decisions(&self) -> Vec<jobs::DispatchDecision> {
        self.job_tracker.read().await.recent_decisions()
    }

    pub async fn candidate_detail(
        &self,
        id: gradient_types::ids::DispatchedJobId,
    ) -> Option<jobs::CandidateDetail> {
        self.job_tracker.read().await.candidate_detail(id)
    }
}
