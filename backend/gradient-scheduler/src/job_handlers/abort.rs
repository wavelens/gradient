/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation abort.

use tracing::info;

use gradient_types::*;

use crate::Scheduler;
use crate::actor::SchedulerMsg;

impl Scheduler {
    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: DB status first, then every worker running one of
    /// its jobs gets `AbortJob` and its pending jobs are dropped.
    pub async fn abort_evaluation(&self, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;
        if let Err(e) = self
            .state
            .graph
            .transition(gradient_graph::Transition::AbortEvaluation {
                evaluation: evaluation_id,
            })
            .await
        {
            tracing::warn!(error = %e, %evaluation_id, "abort did not reach the graph actor");
        }
        for (worker_id, job_id) in self.abort_evaluation_jobs(evaluation_id).await {
            info!(%worker_id, %job_id, %evaluation_id, "sent AbortJob to worker");
        }
    }

    /// The in-memory half of an abort: `(worker, job)` pairs that were told to stop.
    pub async fn abort_evaluation_jobs(
        &self,
        evaluation_id: EvaluationId,
    ) -> Vec<(String, String)> {
        self.call(|reply| SchedulerMsg::AbortEvaluation {
            evaluation_id,
            reply,
        })
        .await
        .unwrap_or_default()
    }

    /// Tell one worker to stop one job; `false` when it is not connected.
    pub async fn abort_job(&self, worker_id: &str, job_id: String, reason: String) -> bool {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::AbortJob {
            worker,
            job_id,
            reason,
            reply,
        })
        .await
        .unwrap_or(false)
    }
}
