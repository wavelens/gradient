/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation abort.

use tracing::info;

use gradient_types::*;

use crate::Scheduler;

impl Scheduler {
    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: update DB status and send `AbortJob` to any
    /// worker currently executing a job belonging to this evaluation.
    pub async fn abort_evaluation(&self, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;

        // Update DB (builds → Aborted, eval → Aborted).
        gradient_db::abort_evaluation(&self.state.db(), evaluation).await;

        // Find active jobs for this evaluation and abort them.
        let tracker = self.job_tracker.read().await;
        let pool = self.worker_pool.read().await;

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
}
