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
use gradient_entity::evaluation_message::MessageLevel;
use gradient_graph::Transition;
use gradient_types::*;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::unbuildable::{Unbuildable, unbuildable_warning};

impl Scheduler {
    // ── Abort ─────────────────────────────────────────────────────────────────

    /// Abort an evaluation: mark it `Aborted` and stop its eval job, then
    /// leave the shared builds to the graph writer in the background. The caller (the
    /// abort button) must not wait on the graph writer's queue; the shared build write
    /// reports which shared builds it moved, and only those builds are stopped -
    /// shared builds another live evaluation still needs keep running for it.
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
            let shared_builds = scheduler
                .abort_evaluation_shared_builds(evaluation_id)
                .await;
            if !shared_builds.is_empty() {
                scheduler
                    .log_aborted_jobs(evaluation_id, shared_builds)
                    .await;
            }
        });
    }

    /// Abort an evaluation parked on systems no connected worker provides and
    /// warn which ones were missing.
    pub(crate) async fn abort_unbuildable_evaluation(&self, unbuildable: Unbuildable) {
        let evaluation_id = unbuildable.evaluation.id;
        let marked = update_evaluation_status(
            &self.state.db(),
            unbuildable.evaluation,
            EvaluationStatus::Aborted,
        )
        .await;
        if marked.status != EvaluationStatus::Aborted {
            return;
        }

        info!(%evaluation_id, unmet = ?unbuildable.unmet, "aborting evaluation: no connected worker provides its systems");
        gradient_db::record_evaluation_message(
            &self.state.db(),
            evaluation_id,
            MessageLevel::Warning,
            unbuildable_warning(&unbuildable.unmet),
            Some("scheduler".to_owned()),
        )
        .await;
        let shared_builds = self.abort_evaluation_shared_builds(evaluation_id).await;
        self.log_aborted_jobs(evaluation_id, shared_builds).await;
    }

    /// Abort the shared builds only `evaluation` still needed; the graph writer owns that write.
    pub(crate) async fn abort_evaluation_shared_builds(
        &self,
        evaluation: EvaluationId,
    ) -> Vec<DerivationBuildId> {
        match self
            .state
            .graph
            .transition(Transition::AbortEvaluationSharedBuilds { evaluation })
            .await
        {
            Ok(report) => report.aborted_shared_builds,
            Err(e) => {
                warn!(error = %e, evaluation_id = %evaluation, "aborting the evaluation's shared builds did not reach the graph writer");
                Vec::new()
            }
        }
    }

    async fn log_aborted_jobs(
        &self,
        evaluation_id: EvaluationId,
        shared_builds: Vec<DerivationBuildId>,
    ) {
        for (worker_id, job_id) in self
            .abort_evaluation_jobs(evaluation_id, shared_builds)
            .await
        {
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
            gradient_db::abandon_open_assignments_for_jobs(&self.state.worker_db, &reaped).await
        {
            warn!(error = %e, "failed to close the dispatch rows of reaped aborts");
        }

        for job_id in reaped.iter().filter(|j| self.attempt_of(j).is_some()) {
            let report = crate::cluster::MemberReport::Aborted { job: None };
            if let Err(e) = self.on_cluster_member_closed(job_id, report).await {
                warn!(error = %e, %job_id, "settling a reaped cluster member failed");
            }
        }

        warn!(jobs = ?reaped, grace_secs = grace.as_secs(), "worker never confirmed the abort; job dropped");
        reaped
    }

    /// The in-memory half of an abort: `(worker, job)` pairs that were told to
    /// stop. `aborted_shared_builds` are the shared builds the database abort moved; a build
    /// on any other shared build is left alone.
    pub async fn abort_evaluation_jobs(
        &self,
        evaluation_id: EvaluationId,
        aborted_shared_builds: Vec<DerivationBuildId>,
    ) -> Vec<(String, String)> {
        self.call(|reply| SchedulerMsg::AbortEvaluation {
            evaluation_id,
            aborted_shared_builds,
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
