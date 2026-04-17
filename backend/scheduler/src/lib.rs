/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job scheduler — tracks connected workers and dispatches eval/build jobs.
//!
//! Injected into the axum router as an `Extension<Arc<Scheduler>>`.

pub mod build;
pub mod ci;
pub mod dispatch;
pub mod eval;
pub mod jobs;
pub mod worker_pool;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use gradient_core::types::*;
use sea_orm::EntityTrait;

use gradient_core::types::proto::{
    BuildOutput, CandidateScore, DiscoveredDerivation, GradientCapabilities, JobCandidate, JobKind,
};

use jobs::{Assignment, JobTracker, PendingBuildJob, PendingEvalJob, PendingJob};
use worker_pool::WorkerPool;

pub use worker_pool::WorkerInfo;

#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod handler_tests;
#[cfg(test)]
mod scheduler_tests;

/// The shared scheduler — clone freely (all fields are `Arc`s).
#[derive(Clone)]
pub struct Scheduler {
    /// Shared application state (DB, CLI config, etc.).
    pub state: Arc<ServerState>,
    worker_pool: Arc<RwLock<WorkerPool>>,
    job_tracker: Arc<RwLock<JobTracker>>,
    /// Signalled when new jobs are enqueued so handler dispatch loops can push
    /// `JobOffer` messages to connected workers.
    job_notify: Arc<tokio::sync::Notify>,
    /// Per-evaluation deferred dependency edges.
    ///
    /// The worker's BFS walks roots→leaves, so batch N may contain a
    /// derivation whose dependency lands in batch N+1. Trying to insert the
    /// edge immediately would FK-fail (dep row doesn't exist yet) or silently
    /// skip (dep not in `drv_path_to_id`). We accumulate `(drv_path,
    /// Vec<dep_drv_path>)` per eval here and flush them all at once in
    /// `handle_eval_job_completed` when every derivation row is guaranteed
    /// to be in the DB.
    deferred_deps: Arc<RwLock<HashMap<Uuid, Vec<(String, Vec<String>)>>>>,
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
        }
    }

    /// Spawn background project polling, eval dispatch, and build dispatch loops.
    ///
    /// Call once after creating the scheduler, before serving requests.
    pub fn start(self: &Arc<Self>) {
        dispatch::start_dispatch_loops(Arc::clone(self));
    }

    // ── Worker lifecycle ──────────────────────────────────────────────────────

    pub async fn is_worker_connected(&self, peer_id: &str) -> bool {
        self.worker_pool.read().await.is_connected(peer_id)
    }

    pub async fn register_worker(
        &self,
        peer_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<Uuid>,
    ) -> (
        Arc<tokio::sync::Notify>,
        tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    ) {
        let (notify, abort_rx) = self
            .worker_pool
            .write()
            .await
            .register(peer_id.to_owned(), capabilities, authorized_peers);
        info!(%peer_id, "worker registered");
        (notify, abort_rx)
    }

    pub async fn update_authorized_peers(&self, peer_id: &str, authorized_peers: HashSet<Uuid>) {
        self.worker_pool
            .write()
            .await
            .update_authorized_peers(peer_id, authorized_peers);
        debug!(%peer_id, "authorized peers updated");
    }

    /// Abort all active jobs on `worker_id` that belong to any of `revoked_peers`.
    /// Jobs are moved back to pending so they can be re-assigned to another worker.
    pub async fn abort_org_jobs_on_worker(&self, worker_id: &str, revoked_peers: &HashSet<Uuid>) {
        if revoked_peers.is_empty() {
            return;
        }
        let job_ids = self
            .job_tracker
            .write()
            .await
            .drain_peer_jobs_on_worker(worker_id, revoked_peers);
        if job_ids.is_empty() {
            return;
        }
        let pool = self.worker_pool.read().await;
        for job_id in &job_ids {
            pool.send_abort(worker_id, job_id.clone(), "org deactivated worker".to_owned());
        }
        info!(
            %worker_id,
            aborted = job_ids.len(),
            "aborted jobs for revoked org(s) on worker"
        );
        // Notify other workers that these jobs are available again.
        self.job_notify.notify_waiters();
    }

    /// Signal a connected worker that its registrations have changed,
    /// triggering a server-initiated re-authentication.
    pub async fn request_reauth(&self, worker_id: &str) {
        self.worker_pool
            .read()
            .await
            .request_reauth(worker_id);
    }

    pub async fn update_worker_capabilities(
        &self,
        peer_id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        self.worker_pool.write().await.update_capabilities(
            peer_id,
            architectures,
            system_features,
            max_concurrent_builds,
        );
        debug!(%peer_id, "worker capabilities updated");
        // Capabilities just changed — a build that was previously "no worker
        // can do this" might now be servable, or vice-versa. Re-evaluate
        // every in-flight evaluation's Waiting/Building gate immediately
        // instead of waiting for the next dispatch tick.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after capability update failed");
        }
    }

    pub async fn unregister_worker(&self, peer_id: &str) {
        let orphaned = self.worker_pool.write().await.unregister(peer_id);
        let tracker_orphaned = self.job_tracker.write().await.worker_disconnected(peer_id);
        let total = orphaned.len() + tracker_orphaned.len();
        if total > 0 {
            info!(%peer_id, orphaned_jobs = total, "worker disconnected; jobs re-queued");
        }
        // A worker leaving may strand evaluations whose remaining builds
        // only it could service.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after worker unregister failed");
        }
    }

    /// Snapshot every connected worker's `(architectures, system_features)`
    /// and reconcile each in-flight evaluation's `Building`/`Waiting` status.
    /// See [`build::reconcile_waiting_state`].
    pub async fn reconcile_waiting_state(&self) -> Result<()> {
        let caps: Vec<(Vec<String>, Vec<String>)> = self
            .worker_pool
            .read()
            .await
            .all_workers()
            .into_iter()
            .map(|w| (w.architectures, w.system_features))
            .collect();
        build::reconcile_waiting_state(&self.state, &caps).await
    }

    pub async fn mark_worker_draining(&self, peer_id: &str) {
        self.worker_pool.write().await.mark_draining(peer_id);
        info!(%peer_id, "worker marked draining");
    }

    // ── Job queue ─────────────────────────────────────────────────────────────

    pub async fn enqueue_eval_job(&self, job_id: String, job: PendingEvalJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id, PendingJob::Eval(job));
        self.job_notify.notify_waiters();
        candidate
    }

    pub async fn enqueue_build_job(&self, job_id: String, job: PendingBuildJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id, PendingJob::Build(job));
        self.job_notify.notify_waiters();
        candidate
    }

    /// Returns a handle that handler dispatch loops can `await` on to be woken
    /// when new jobs are enqueued in the scheduler.
    pub fn job_notify(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.job_notify)
    }

    /// Returns ALL pending job candidates visible to the given worker.
    /// Used for `RequestJobList` / `RequestAllCandidates` (full snapshot).
    /// Also marks all returned IDs as sent so delta pushes skip them.
    pub async fn get_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let (authorized, caps) = {
            let pool = self.worker_pool.read().await;
            let auth = pool.authorized_peers_for(worker_id);
            let authorized = if auth.is_some_and(|p| !p.is_empty()) {
                auth.cloned()
            } else {
                None
            };
            let caps = pool.build_caps_for(worker_id).map(|(a, f)| {
                jobs::WorkerBuildCaps {
                    architectures: a,
                    system_features: f,
                }
            });
            (authorized, caps)
        };
        let candidates = self
            .job_tracker
            .read()
            .await
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        // Mark all as sent so the next delta push skips them.
        let ids: Vec<String> = candidates.iter().map(|c| c.job_id.clone()).collect();
        self.worker_pool
            .write()
            .await
            .mark_candidates_sent(worker_id, &ids);
        candidates
    }

    /// Returns only NEW pending job candidates (not yet sent to this worker).
    /// Used for incremental `JobOffer` pushes after `job_notify`.
    /// Marks the returned IDs as sent.
    pub async fn get_new_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let (authorized, sent, caps) = {
            let pool = self.worker_pool.read().await;
            let auth = pool.authorized_peers_for(worker_id);
            let authorized = if auth.is_some_and(|p| !p.is_empty()) {
                auth.cloned()
            } else {
                None
            };
            let sent = pool
                .sent_candidates_for(worker_id)
                .cloned()
                .unwrap_or_default();
            let caps = pool.build_caps_for(worker_id).map(|(a, f)| {
                jobs::WorkerBuildCaps {
                    architectures: a,
                    system_features: f,
                }
            });
            (authorized, sent, caps)
        };
        let all = self
            .job_tracker
            .read()
            .await
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        let new_candidates: Vec<JobCandidate> = all
            .into_iter()
            .filter(|c| !sent.contains(&c.job_id))
            .collect();
        if !new_candidates.is_empty() {
            let ids: Vec<String> = new_candidates.iter().map(|c| c.job_id.clone()).collect();
            self.worker_pool
                .write()
                .await
                .mark_candidates_sent(worker_id, &ids);
        }
        new_candidates
    }

    // ── Scoring / assignment ──────────────────────────────────────────────────

    /// Try to directly assign a job of `kind` to `peer_id` without scoring.
    ///
    /// Called when the worker sends `RequestJob { kind }` to signal it has a
    /// free slot.  Returns `Some(Assignment)` if a matching pending job was
    /// found and claimed; `None` if no such job exists yet.
    pub async fn request_job(&self, peer_id: &str, kind: JobKind) -> Option<Assignment> {
        // ── Server-side capacity guard ──────────────────────────────────────
        {
            let pool = self.worker_pool.read().await;
            if !pool.has_capacity(peer_id, &kind) {
                debug!(%peer_id, ?kind, "RequestJob ignored — worker at capacity");
                return None;
            }
        }

        let (authorized, caps) = {
            let pool = self.worker_pool.read().await;
            let authorized: Option<HashSet<Uuid>> = pool
                .authorized_peers_for(peer_id)
                .and_then(|p| if p.is_empty() { None } else { Some(p.clone()) });
            let caps = pool.build_caps_for(peer_id).map(|(a, f)| {
                jobs::WorkerBuildCaps {
                    architectures: a,
                    system_features: f,
                }
            });
            (authorized, caps)
        };

        // ── First try: pick from what's already in the tracker ──────────────
        let assignment = self
            .job_tracker
            .write()
            .await
            .take_best_of_kind(peer_id, authorized.as_ref(), caps.as_ref(), &kind);
        if let Some(ref a) = assignment {
            self.worker_pool
                .write()
                .await
                .assign_job(peer_id, &a.job_id);
            info!(%peer_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob");
            return assignment;
        }

        // ── Tracker empty: on-demand DB refresh ─────────────────────────────
        // Build dispatch is demand-driven: instead of a 5-second polling
        // loop that blocks the handler, we only query the DB when a worker
        // actually asks for work and the tracker has nothing. This is the
        // ONLY place dispatch_ready_builds runs for build jobs.
        if matches!(kind, JobKind::Build) {
            if let Err(e) = dispatch::dispatch_ready_builds(self).await {
                warn!(error = %e, "on-demand dispatch_ready_builds failed");
            }
            // Also reconcile Waiting/Building state while we're at it.
            if let Err(e) = self.reconcile_waiting_state().await {
                warn!(error = %e, "reconcile_waiting_state after on-demand dispatch failed");
            }
        }

        // ── Second try after refresh ────────────────────────────────────────
        let assignment = self
            .job_tracker
            .write()
            .await
            .take_best_of_kind(peer_id, authorized.as_ref(), caps.as_ref(), &kind);
        if let Some(ref a) = assignment {
            self.worker_pool
                .write()
                .await
                .assign_job(peer_id, &a.job_id);
            info!(%peer_id, job_id = %a.job_id, ?kind, "job assigned via RequestJob (after DB refresh)");
        }
        assignment
    }

    /// Record candidate scores from a worker. Does NOT assign — the worker
    /// explicitly signals capacity via `RequestJob`. Scores are used later
    /// by `request_job` to pick the best candidate.
    pub async fn record_scores(&self, peer_id: &str, scores: Vec<CandidateScore>) {
        self.job_tracker
            .write()
            .await
            .record_scores(peer_id, scores);
    }

    pub async fn job_rejected(&self, peer_id: &str, job_id: &str) {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        self.job_tracker.write().await.release_to_pending(job_id);
        // Clear the sent-candidate flag so the job shows up in the next delta push.
        self.worker_pool
            .write()
            .await
            .remove_sent_candidate(job_id);
        info!(%peer_id, %job_id, "job rejected; re-queued");
    }

    // ── Status updates ────────────────────────────────────────────────────────

    // ── Eval status transitions ───────────────────────────────────────────────

    pub async fn handle_eval_status_update(
        &self,
        job_id: &str,
        new_status: entity::evaluation::EvaluationStatus,
    ) {
        let (evaluation_id, project_id) = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => (j.evaluation_id, j.project_id),
                _ => return,
            }
        };
        match EEvaluation::find_by_id(evaluation_id)
            .one(&self.state.db)
            .await
        {
            Ok(Some(eval)) => {
                // Report CI "Running" when evaluation starts (first status transition).
                if new_status == entity::evaluation::EvaluationStatus::Fetching
                    && let Some(pid) = project_id
                {
                    let state = Arc::clone(&self.state);
                    let repo = eval.repository.clone();
                    let commit_id = eval.commit;
                    tokio::spawn(async move {
                        ci::report_ci_for_evaluation(
                            state,
                            pid,
                            commit_id,
                            &repo,
                            evaluation_id,
                            gradient_core::ci::CiStatus::Running,
                        )
                        .await;
                    });
                }
                gradient_core::db::update_evaluation_status(
                    Arc::clone(&self.state),
                    eval,
                    new_status,
                )
                .await;
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => {
                warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update")
            }
        }
    }

    pub async fn handle_build_status_update(&self, build_id_str: &str) {
        let build_id = match build_id_str.parse::<Uuid>() {
            Ok(id) => id,
            Err(_) => {
                warn!(%build_id_str, "invalid build_id in Building update");
                return;
            }
        };
        use entity::build::BuildStatus;
        match EBuild::find_by_id(build_id).one(&self.state.db).await {
            Ok(Some(build)) => {
                gradient_core::db::update_build_status(
                    Arc::clone(&self.state),
                    build,
                    BuildStatus::Building,
                )
                .await;
            }
            Ok(None) => warn!(%build_id, "build not found for Building status update"),
            Err(e) => warn!(error = %e, %build_id, "failed to fetch build for status update"),
        }
    }

    pub async fn handle_eval_result(
        &self,
        job_id: &str,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not an eval job", job_id),
                None => {
                    warn!(%job_id, "eval result for unknown job — ignoring");
                    return Ok(());
                }
            }
        };

        // Accumulate dep edges to flush later (at eval-job-completed) when
        // every derivation row is guaranteed to be in the DB. The BFS walks
        // roots→leaves, so batch N may contain a derivation whose dep lands
        // in batch N+1 — inserting the edge now would FK-fail or silently
        // skip the dep because the row doesn't exist yet.
        let eval_id = job.evaluation_id;
        let dep_pairs: Vec<(String, Vec<String>)> = derivations
            .iter()
            .filter(|d| !d.dependencies.is_empty())
            .map(|d| (d.drv_path.clone(), d.dependencies.clone()))
            .collect();
        if !dep_pairs.is_empty() {
            self.deferred_deps
                .write()
                .await
                .entry(eval_id)
                .or_default()
                .extend(dep_pairs);
        }

        eval::handle_eval_result(&self.state, &job, derivations, warnings, errors).await
    }

    pub async fn handle_build_output(
        &self,
        job_id: &str,
        build_id_str: &str,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        let build_id = build_id_str
            .parse::<Uuid>()
            .map_err(|_| anyhow::anyhow!("invalid build_id: {}", build_id_str))?;

        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not a build job", job_id),
                None => {
                    warn!(%job_id, "build output for unknown job — ignoring");
                    return Ok(());
                }
            }
        };
        build::handle_build_output(&self.state, &job, build_id, outputs).await
    }

    // ── Job completion ────────────────────────────────────────────────────────

    pub async fn handle_job_completed(&self, peer_id: &str, job_id: &str) -> Result<()> {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
            Some(PendingJob::Eval(j)) => {
                // Flush deferred dependency edges BEFORE promoting builds,
                // so the dispatch SQL's dep-gating sees the full graph.
                let deferred = self
                    .deferred_deps
                    .write()
                    .await
                    .remove(&j.evaluation_id)
                    .unwrap_or_default();
                if let Err(e) =
                    eval::flush_deferred_deps(&self.state, j.evaluation_id, j.peer_id, deferred)
                        .await
                {
                    error!(error = %e, evaluation_id = %j.evaluation_id, "flush_deferred_deps failed");
                }
                eval::handle_eval_job_completed(&self.state, j.evaluation_id).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_completed(&self.state, j.build_id).await
                // Same: the worker chains a RequestJob after JobCompleted,
                // which triggers on-demand dispatch if needed.
            }
            None => {
                warn!(%job_id, "job_completed for unknown job");
                Ok(())
            }
        }
    }

    pub async fn handle_job_failed(&self, peer_id: &str, job_id: &str, error: &str) -> Result<()> {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
            Some(PendingJob::Eval(j)) => {
                eval::handle_eval_job_failed(&self.state, j.evaluation_id, error).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_failed(&self.state, j.build_id, error).await
            }
            None => {
                warn!(%job_id, "job_failed for unknown job");
                Ok(())
            }
        }
    }

    // ── Log streaming ─────────────────────────────────────────────────────────

    pub async fn append_log(&self, job_id: &str, task_index: u32, data: Vec<u8>) -> Result<()> {
        let bytes_len = data.len();
        let text = match std::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => {
                debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: non-UTF-8");
                return Ok(());
            }
        };

        let build_id_str = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j
                    .job
                    .builds
                    .get(task_index as usize)
                    .map(|t| t.build_id.clone()),
                Some(PendingJob::Eval(_)) => {
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: job is an eval, not a build");
                    return Ok(());
                }
                None => {
                    // Common shortly after a build finishes: a few in-flight
                    // log lines from the worker arrive after the job has
                    // already been removed from the active tracker. Not an
                    // error — just lost output, log at debug.
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: no active job (likely race with completion)");
                    return Ok(());
                }
            }
        };

        let build_id = match build_id_str.and_then(|s| s.parse::<Uuid>().ok()) {
            Some(id) => id,
            None => {
                warn!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: build_task index out of range or build_id unparseable");
                return Ok(());
            }
        };

        let log_id = match EBuild::find_by_id(build_id).one(&self.state.db).await? {
            Some(b) => b.log_id.unwrap_or(b.id),
            None => build_id,
        };

        debug!(%build_id, %log_id, bytes = bytes_len, "appending build log");
        self.state.log_storage.append(log_id, text).await
    }

    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: update DB status and send `AbortJob` to any
    /// worker currently executing a job belonging to this evaluation.
    pub async fn abort_evaluation(&self, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;

        // Update DB (builds → Aborted, eval → Aborted).
        gradient_core::db::abort_evaluation(Arc::clone(&self.state), evaluation).await;

        // Find active jobs for this evaluation and abort them.
        let tracker = self.job_tracker.read().await;
        let pool = self.worker_pool.read().await;

        // Collect (worker_id, job_id) pairs for jobs belonging to this evaluation.
        // We need to iterate active jobs to find eval/build jobs for this evaluation.
        let to_abort: Vec<(String, String)> = tracker
            .active_jobs()
            .filter_map(|(job_id, worker_id, job)| {
                if job.evaluation_id() == evaluation_id {
                    Some((worker_id.to_owned(), job_id.to_owned()))
                } else {
                    None
                }
            })
            .collect();

        for (worker_id, job_id) in to_abort {
            if pool.send_abort(&worker_id, job_id.clone(), "evaluation aborted".to_owned()) {
                info!(%worker_id, %job_id, %evaluation_id, "sent AbortJob to worker");
            }
        }

        // Also remove any pending (unassigned) jobs for this evaluation.
        drop(pool);
        drop(tracker);
        self.job_tracker
            .write()
            .await
            .remove_pending_for_evaluation(evaluation_id);
    }

    /// Return the peer (org) UUID that owns the active job, if found.
    pub async fn peer_id_for_job(&self, job_id: &str) -> Option<Uuid> {
        self.job_tracker
            .read()
            .await
            .active_job(job_id)
            .map(|j| j.peer_id())
    }

    // ── Diagnostics ───────────────────────────────────────────────────────────

    pub async fn worker_count(&self) -> usize {
        self.worker_pool.read().await.worker_count()
    }

    pub async fn workers_info(&self) -> Vec<WorkerInfo> {
        self.worker_pool.read().await.all_workers()
    }

    pub async fn pending_job_count(&self) -> usize {
        self.job_tracker.read().await.pending_count()
    }
}
