/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_db::status::{BuildRefusal, EvaluationWarning, update_evaluation_status};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_types::*;
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::unbuildable::Unbuildable;

fn aborted(evaluation_id: EvaluationId, marked: Result<MEvaluation, sea_orm::DbErr>) -> bool {
    match marked {
        Ok(evaluation) => evaluation.status == EvaluationStatus::Aborted,
        Err(e) => {
            warn!(error = %e, %evaluation_id, "failed to mark the evaluation aborted");
            false
        }
    }
}

impl Scheduler {
    /// The abort button must not wait on the graph writer's queue. Only the shared builds the
    /// database abort moved are stopped. Shared builds still wanted by another live evaluation keep
    /// running.
    pub async fn abort_evaluation(self: &Arc<Self>, evaluation: MEvaluation) {
        let evaluation_id = evaluation.id;
        let marked =
            update_evaluation_status(&self.db, evaluation, EvaluationStatus::Aborted).await;
        if !aborted(evaluation_id, marked) {
            return;
        }

        self.log_aborted_jobs(evaluation_id, Vec::new()).await;

        let scheduler = Arc::clone(self);
        self.db.shutdown.spawn(async move {
            let shared_builds = scheduler
                .abort_evaluation_shared_builds(evaluation_id, true)
                .await;
            if !shared_builds.is_empty() {
                let job_ids = shared_builds
                    .into_iter()
                    .map(crate::jobs::build_job_key)
                    .collect();
                scheduler.cancel_jobs(evaluation_id, job_ids).await;
            }
            scheduler.refresh_pre_warm().await;
        });
    }

    pub async fn stop_aborted_evaluation(&self, evaluation: EvaluationId, hard_abort: bool) {
        self.drop_import_waits(evaluation).await;
        self.drop_input_waits(evaluation);
        let shared_builds = self
            .abort_evaluation_shared_builds(evaluation, hard_abort)
            .await;

        self.cancel_evaluation_jobs(evaluation, &shared_builds)
            .await;
    }

    /// Stop the worker only after the graph writer moved the shared build.
    pub async fn abort_build(
        &self,
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    ) -> anyhow::Result<Result<(), BuildRefusal>> {
        let report = self
            .graph
            .transition(Transition::AbortBuild {
                evaluation,
                shared_build,
            })
            .await?;
        if let Some(refusal) = report.refusal {
            return Ok(Err(refusal));
        }

        let job_ids = report
            .aborted_shared_builds
            .into_iter()
            .map(crate::jobs::build_job_key)
            .collect();
        self.cancel_jobs(evaluation, job_ids).await;
        Ok(Ok(()))
    }

    pub async fn retry_build(
        &self,
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    ) -> anyhow::Result<Result<(), BuildRefusal>> {
        if self
            .active_job(&crate::jobs::build_job_key(shared_build))
            .await
            .is_some()
        {
            return Ok(Err(BuildRefusal::WorkerStillStopping));
        }

        let report = self
            .graph
            .transition(Transition::RetryBuild {
                evaluation,
                shared_build,
            })
            .await?;
        Ok(report.refusal.map_or(Ok(()), Err))
    }

    pub(crate) async fn abort_unbuildable_evaluation(&self, unbuildable: Unbuildable) {
        let evaluation_id = unbuildable.evaluation.id;
        let marked =
            update_evaluation_status(&self.db, unbuildable.evaluation, EvaluationStatus::Aborted)
                .await;
        if !aborted(evaluation_id, marked) {
            return;
        }

        info!(%evaluation_id, unmet = ?unbuildable.unmet, "aborting evaluation: no connected worker provides its systems");
        EvaluationWarning::NoWorker {
            unmet: &unbuildable.unmet,
        }
        .report(&self.db.worker_db, evaluation_id)
        .await;

        let shared_builds = self
            .abort_evaluation_shared_builds(evaluation_id, true)
            .await;
        self.log_aborted_jobs(evaluation_id, shared_builds).await;
    }

    pub(crate) async fn abort_evaluation_shared_builds(
        &self,
        evaluation: EvaluationId,
        hard: bool,
    ) -> Vec<DerivationBuildId> {
        match self
            .graph
            .transition(Transition::AbortEvaluationSharedBuilds { evaluation, hard })
            .await
        {
            Ok(report) => {
                if !report.aborted_runner_jobs.is_empty() {
                    let keys = report
                        .aborted_runner_jobs
                        .iter()
                        .copied()
                        .map(crate::jobs::runner_job_key)
                        .collect();
                    self.cancel_jobs(evaluation, keys).await;
                }
                report.aborted_shared_builds
            }
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

    /// A later report from the worker is finding no job and changing nothing.
    pub async fn reap_overdue_aborts(&self, grace: Duration) -> Vec<String> {
        let reaped = self
            .call(|reply| SchedulerMsg::ReapOverdueAborts { grace, reply })
            .await
            .unwrap_or_default();
        if reaped.is_empty() {
            return reaped;
        }

        if let Err(e) =
            gradient_db::scheduling::assignment_record::abandon_open_assignments_for_jobs(
                &self.db.worker_db,
                &reaped,
            )
            .await
        {
            warn!(error = %e, "failed to close the dispatch rows of reaped aborts");
        }

        for job_id in &reaped {
            if self.cluster.book.attempt_of(job_id).await.is_none() {
                continue;
            }
            let report = crate::cluster::MemberReport::Aborted { job: None };
            if let Err(e) = self.on_cluster_member_closed(job_id, report).await {
                warn!(error = %e, %job_id, "settling a reaped cluster member failed");
            }
        }

        warn!(jobs = ?reaped, grace_secs = grace.as_secs(), "worker never confirmed the abort; job dropped");
        reaped
    }

    pub async fn abort_evaluation_jobs(
        &self,
        evaluation_id: EvaluationId,
        aborted_shared_builds: Vec<DerivationBuildId>,
    ) -> Vec<(String, String)> {
        self.drop_import_waits(evaluation_id).await;
        self.drop_input_waits(evaluation_id);
        self.call(|reply| SchedulerMsg::AbortEvaluation {
            evaluation_id,
            aborted_shared_builds,
            reply,
        })
        .await
        .unwrap_or_default()
    }

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
