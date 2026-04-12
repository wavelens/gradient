/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Proto scheduler — tracks connected workers and dispatches jobs.
//!
//! Lives inside the `proto` crate to share message types without a circular
//! dependency. Injected into the axum router as an `Extension<Arc<Scheduler>>`.

pub mod build;
pub mod dispatch;
pub mod eval;
pub mod jobs;
pub mod worker_pool;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use gradient_core::types::*;
use sea_orm::EntityTrait;

use crate::messages::{
    BuildOutput, CandidateScore, DiscoveredDerivation, GradientCapabilities, JobCandidate,
};

use jobs::{Assignment, JobTracker, PendingBuildJob, PendingEvalJob, PendingJob};
use worker_pool::WorkerPool;

pub use worker_pool::WorkerInfo;

/// The shared scheduler — clone freely (all fields are `Arc`s).
#[derive(Clone)]
pub struct Scheduler {
    pub(crate) state: Arc<ServerState>,
    worker_pool: Arc<RwLock<WorkerPool>>,
    job_tracker: Arc<RwLock<JobTracker>>,
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
        }
    }

    /// Spawn background eval/build dispatch loops.
    ///
    /// Call once after creating the scheduler, before serving requests.
    pub fn start(self: &Arc<Self>) {
        dispatch::start_dispatch_loops(Arc::clone(self));
    }

    // ── Worker lifecycle ──────────────────────────────────────────────────────

    pub async fn register_worker(&self, peer_id: &str, capabilities: GradientCapabilities) {
        self.worker_pool.write().await.register(peer_id.to_owned(), capabilities);
        info!(%peer_id, "worker registered");
    }

    pub async fn update_worker_capabilities(
        &self,
        peer_id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        self.worker_pool
            .write()
            .await
            .update_capabilities(peer_id, architectures, system_features, max_concurrent_builds);
        debug!(%peer_id, "worker capabilities updated");
    }

    pub async fn unregister_worker(&self, peer_id: &str) {
        let orphaned = self.worker_pool.write().await.unregister(peer_id);
        let tracker_orphaned = self.job_tracker.write().await.worker_disconnected(peer_id);
        let total = orphaned.len() + tracker_orphaned.len();
        if total > 0 {
            info!(%peer_id, orphaned_jobs = total, "worker disconnected; jobs re-queued");
        }
    }

    pub async fn mark_worker_draining(&self, peer_id: &str) {
        self.worker_pool.write().await.mark_draining(peer_id);
        info!(%peer_id, "worker marked draining");
    }

    // ── Job queue ─────────────────────────────────────────────────────────────

    pub async fn enqueue_eval_job(&self, job_id: String, job: PendingEvalJob) -> JobCandidate {
        self.job_tracker.write().await.add_pending(job_id, PendingJob::Eval(job))
    }

    pub async fn enqueue_build_job(&self, job_id: String, job: PendingBuildJob) -> JobCandidate {
        self.job_tracker.write().await.add_pending(job_id, PendingJob::Build(job))
    }

    pub async fn get_job_candidates(&self) -> Vec<JobCandidate> {
        self.job_tracker.read().await.all_candidates()
    }

    // ── Scoring / assignment ──────────────────────────────────────────────────

    pub async fn consider_scores(
        &self,
        peer_id: &str,
        scores: Vec<CandidateScore>,
    ) -> Option<Assignment> {
        // Try score-based assignment (missing: 0).
        let assignment = self.job_tracker.write().await.receive_scores(peer_id, scores);
        if let Some(ref a) = assignment {
            self.worker_pool.write().await.assign_job(peer_id, &a.job_id);
            info!(%peer_id, job_id = %a.job_id, "job assigned via scoring");
            return assignment;
        }
        // Fallback: assign any job with no required paths.
        let fallback = self.job_tracker.write().await.take_empty_required(peer_id);
        if let Some(ref a) = fallback {
            self.worker_pool.write().await.assign_job(peer_id, &a.job_id);
            info!(%peer_id, job_id = %a.job_id, "job assigned (no required paths)");
        }
        fallback
    }

    pub async fn job_rejected(&self, peer_id: &str, job_id: &str) {
        self.worker_pool.write().await.release_job(peer_id, job_id);
        self.job_tracker.write().await.release_to_pending(job_id);
        info!(%peer_id, %job_id, "job rejected; re-queued");
    }

    // ── Status updates ────────────────────────────────────────────────────────

    // ── Eval status transitions ───────────────────────────────────────────────

    pub async fn handle_eval_status_update(
        &self,
        job_id: &str,
        new_status: entity::evaluation::EvaluationStatus,
    ) {
        let evaluation_id = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Eval(j)) => j.evaluation_id,
                _ => return,
            }
        };
        match EEvaluation::find_by_id(evaluation_id).one(&self.state.db).await {
            Ok(Some(eval)) => {
                gradient_core::db::update_evaluation_status(Arc::clone(&self.state), eval, new_status).await;
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update"),
        }
    }

    pub async fn handle_build_status_update(&self, build_id_str: &str) {
        let build_id = match build_id_str.parse::<Uuid>() {
            Ok(id) => id,
            Err(_) => { warn!(%build_id_str, "invalid build_id in Building update"); return; }
        };
        use entity::build::BuildStatus;
        match EBuild::find_by_id(build_id).one(&self.state.db).await {
            Ok(Some(build)) => {
                gradient_core::db::update_build_status(Arc::clone(&self.state), build, BuildStatus::Building).await;
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
        eval::handle_eval_result(&self.state, &job, derivations, warnings).await
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
                eval::handle_eval_job_completed(&self.state, j.evaluation_id).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_completed(&self.state, j.build_id).await
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
        let text = match std::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => return Ok(()), // drop non-UTF-8 chunks silently
        };

        let build_id_str = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => {
                    j.job.builds.get(task_index as usize)
                        .map(|t| t.build_id.clone())
                }
                _ => None,
            }
        };

        let build_id = match build_id_str.and_then(|s| s.parse::<Uuid>().ok()) {
            Some(id) => id,
            None => return Ok(()), // eval job or missing task — drop
        };

        let log_id = match EBuild::find_by_id(build_id).one(&self.state.db).await? {
            Some(b) => b.log_id.unwrap_or(b.id),
            None => build_id,
        };

        self.state.log_storage.append(log_id, text).await
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
