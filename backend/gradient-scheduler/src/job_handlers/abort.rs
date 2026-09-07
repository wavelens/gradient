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
    /// its jobs gets `AbortJob` and its pending jobs are dropped. The write
    /// reports which anchors it moved, and only those builds are stopped -
    /// anchors another live evaluation still needs keep running for it.
    pub async fn abort_evaluation(&self, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;
        let aborted_anchors = match self
            .state
            .graph
            .transition(gradient_graph::Transition::AbortEvaluation {
                evaluation: evaluation_id,
            })
            .await
        {
            Ok(report) => report.aborted_anchors,
            Err(e) => {
                tracing::warn!(error = %e, %evaluation_id, "abort did not reach the graph actor");
                Vec::new()
            }
        };

        for (worker_id, job_id) in self
            .abort_evaluation_jobs(evaluation_id, aborted_anchors)
            .await
        {
            info!(%worker_id, %job_id, %evaluation_id, "sent AbortJob to worker");
        }
    }

    /// The in-memory half of an abort: `(worker, job)` pairs that were told to
    /// stop. `aborted_anchors` are the anchors the database abort moved; a build
    /// on any other anchor is left alone.
    pub async fn abort_evaluation_jobs(
        &self,
        evaluation_id: EvaluationId,
        aborted_anchors: Vec<DerivationBuildId>,
    ) -> Vec<(String, String)> {
        self.call(|reply| SchedulerMsg::AbortEvaluation {
            evaluation_id,
            aborted_anchors,
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
