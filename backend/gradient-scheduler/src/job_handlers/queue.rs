/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job queue: enqueue, candidate listing, and diagnostics.

use gradient_types::proto::JobCandidate;

use crate::Scheduler;
use crate::jobs::{PendingBuildJob, PendingEvalJob, PendingJob};
use crate::worker_pool::WorkerInfo;

impl Scheduler {
    // ── Job queue ─────────────────────────────────────────────────────────────

    pub async fn enqueue_eval_job(&self, job_id: String, job: PendingEvalJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id, PendingJob::Eval(job));
        self.job_notify.send_modify(|g| *g = g.wrapping_add(1));
        candidate
    }

    pub async fn enqueue_build_job(&self, job_id: String, job: PendingBuildJob) -> JobCandidate {
        let candidate = self
            .job_tracker
            .write()
            .await
            .add_pending(job_id.clone(), PendingJob::Build(job));
        // A re-queued build (after a failed/rejected/orphaned dispatch) must be
        // offered again so workers score it a second time; clear stale
        // sent-candidate flags. No-op on a first enqueue.
        self.worker_pool.write().await.remove_sent_candidate(&job_id);
        self.job_notify.send_modify(|g| *g = g.wrapping_add(1));
        candidate
    }

    /// Returns a `watch` receiver handler dispatch loops `await` (via
    /// `changed()`) to learn that new jobs were enqueued. Level-triggered: a
    /// bump fired while the session is busy is seen on its next `changed()`.
    pub fn job_notify(&self) -> tokio::sync::watch::Receiver<u64> {
        self.job_notify.subscribe()
    }

    /// Wake `build_dispatch_loop` now instead of waiting for its 5s tick. Called
    /// when a job completes and leaves its worker idle, so the dependents it just
    /// unblocked are enqueued and offered immediately - collapsing per-level
    /// latency on serial chains without kicking while the worker is still busy.
    pub(crate) fn kick_dispatch(&self) {
        self.dispatch_kick.notify_one();
    }

    /// Returns ALL pending job candidates visible to the given worker.
    /// Used for `RequestJobList` / `RequestAllCandidates` (full snapshot).
    /// Also marks all returned IDs as sent so delta pushes skip them.
    pub async fn get_job_candidates(&self, worker_id: &str) -> Vec<JobCandidate> {
        let (authorized, caps) = self.worker_auth_and_caps(worker_id).await;
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
        let (authorized, caps) = self.worker_auth_and_caps(worker_id).await;
        let sent = {
            let pool = self.worker_pool.read().await;
            pool.sent_candidates_for(worker_id)
                .cloned()
                .unwrap_or_default()
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

    /// Returns the connected worker's negotiated `GradientCapabilities`,
    /// or `None` if the worker is not connected.
    pub async fn worker_gradient_caps(
        &self,
        worker_id: &str,
    ) -> Option<gradient_types::proto::GradientCapabilities> {
        self.worker_pool.read().await.gradient_caps_for(worker_id)
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
