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
pub mod ci;
pub mod dispatch;
pub mod eval;
pub mod jobs;
pub mod log_substitution;
pub mod peer_auth;
pub mod policy;
pub mod worker_pool;
pub mod worker_state;

mod job_handlers;
pub(crate) mod trigger_dispatch;
mod worker_lifecycle;

use std::collections::HashMap;
use std::sync::Arc;

use gradient_core::types::*;
use tokio::sync::RwLock;

use jobs::JobTracker;
use policy::Policy;
use worker_pool::WorkerPool;

/// Per-evaluation deferred dependency edges accumulated during eval result
/// processing and flushed once all derivation rows are in the DB.
type DeferredDeps = Arc<RwLock<HashMap<EvaluationId, Vec<(String, Vec<String>)>>>>;

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
    /// Signalled when new jobs are enqueued so handler dispatch loops can push
    /// `JobOffer` messages to connected workers.
    pub(crate) job_notify: Arc<tokio::sync::Notify>,
    /// Per-evaluation deferred dependency edges.
    ///
    /// The worker's BFS walks roots→leaves, so batch N may contain a
    /// derivation whose dependency lands in batch N+1. Trying to insert the
    /// edge immediately would FK-fail (dep row doesn't exist yet) or silently
    /// skip (dep not in `drv_path_to_id`). We accumulate `(drv_path,
    /// Vec<dep_drv_path>)` per eval here and flush them all at once in
    /// `handle_eval_job_completed` when every derivation row is guaranteed
    /// to be in the DB.
    pub(crate) deferred_deps: DeferredDeps,
    /// Scoring policy used when selecting which pending job to assign to a
    /// requesting worker.  Shared via `Arc` so it can be read lock-free.
    pub(crate) policy: Arc<Policy>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(state: Arc<ServerState>) -> Self {
        Self {
            state,
            worker_pool: Arc::new(RwLock::new(WorkerPool::new())),
            job_tracker: Arc::new(RwLock::new(JobTracker::new())),
            job_notify: Arc::new(tokio::sync::Notify::new()),
            deferred_deps: Arc::new(RwLock::new(HashMap::new())),
            policy: Arc::new(Policy::default_build_policy()),
        }
    }

    /// Drop the eval job and any associated build jobs from the in-memory
    /// tracker. Workers that have already been assigned will finish or time out
    /// normally; the DB-side abort (via `core::ci::abort_evaluation`) is the
    /// caller's responsibility.
    pub async fn cancel_evaluation_jobs(&self, eval_id: EvaluationId, build_ids: &[BuildId]) {
        let mut tracker = self.job_tracker.write().await;
        tracker.remove_job(&format!("eval:{eval_id}"));
        for bid in build_ids {
            tracker.remove_job(&format!("build:{bid}"));
        }
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
}
