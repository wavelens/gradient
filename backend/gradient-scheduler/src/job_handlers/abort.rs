/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation abort.

use std::sync::Arc;
use std::time::Duration;

use gradient_db::update_evaluation_status;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_types::*;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;

impl Scheduler {
    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: mark it `Aborted` and stop its eval job, then
    /// leave the anchors to the graph actor in the background. The caller (the
    /// abort button) must not wait on the graph actor's queue; the anchor write
    /// reports which anchors it moved, and only those builds are stopped -
    /// anchors another live evaluation still needs keep running for it.
    pub async fn abort_evaluation(self: &Arc<Self>, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;
        let marked =
            update_evaluation_status(&self.state.db(), evaluation, EvaluationStatus::Aborted).await;
        if marked.status != EvaluationStatus::Aborted {
            return;
        }

        self.log_aborted_jobs(evaluation_id, Vec::new()).await;

        let scheduler = Arc::clone(self);
        self.state.shutdown.spawn(async move {
            let anchors = scheduler.abort_evaluation_anchors(evaluation_id).await;
            if !anchors.is_empty() {
                scheduler.log_aborted_jobs(evaluation_id, anchors).await;
            }
        });
    }

    /// Abort the anchors only `evaluation` still needed; the graph actor owns that write.
    pub(crate) async fn abort_evaluation_anchors(
        &self,
        evaluation: EvaluationId,
    ) -> Vec<DerivationBuildId> {
        match self
            .state
            .graph
            .transition(Transition::AbortEvaluationAnchors { evaluation })
            .await
        {
            Ok(report) => report.aborted_anchors,
            Err(e) => {
                warn!(error = %e, evaluation_id = %evaluation, "aborting the evaluation's anchors did not reach the graph actor");
                Vec::new()
            }
        }
    }

    async fn log_aborted_jobs(&self, evaluation_id: EvaluationId, anchors: Vec<DerivationBuildId>) {
        for (worker_id, job_id) in self.abort_evaluation_jobs(evaluation_id, anchors).await {
            info!(%worker_id, %job_id, %evaluation_id, "sent AbortJob to worker");
        }
    }

    /// Drop the jobs whose worker has not confirmed an abort within `grace` and
    /// close their dispatch rows, returning the job ids. A report the worker
    /// sends later finds no job and changes nothing.
    pub async fn reap_overdue_aborts(&self, grace: Duration) -> Vec<String> {
        let reaped = self
            .call(|reply| SchedulerMsg::ReapOverdueAborts { grace, reply })
            .await
            .unwrap_or_default();
        if reaped.is_empty() {
            return reaped;
        }

        if let Err(e) =
            gradient_db::abandon_open_dispatches_for_jobs(&self.state.worker_db, &reaped).await
        {
            warn!(error = %e, "failed to close the dispatch rows of reaped aborts");
        }

        warn!(jobs = ?reaped, grace_secs = grace.as_secs(), "worker never confirmed the abort; job dropped");
        reaped
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
