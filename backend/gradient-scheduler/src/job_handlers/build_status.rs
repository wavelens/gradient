/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::Result;
use sea_orm::EntityTrait;
use tracing::{info, warn};

use gradient_graph::Transition;
use gradient_types::*;
use gradient_wire::types::{BuildFailureKind, BuildMetrics, BuildOutput};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::cluster::{Failure, MemberReport};
use crate::jobs::PendingJob;

impl Scheduler {
    pub async fn handle_build_status_update(&self, build_id_str: &str, worker_id: &str) {
        let derivation_build = match build_id_str.parse::<DerivationBuildId>() {
            Ok(id) => id,
            Err(_) => {
                warn!(%build_id_str, "invalid derivation_build in Building update");
                return;
            }
        };

        match self
            .state
            .graph
            .transition(Transition::BuildStarted {
                shared_build: derivation_build,
            })
            .await
        {
            // This is the backstop for the dispatch/abort race. A shared build dispatched just
            // before its evaluation was aborted can still report started here. The worker is told
            // to stop instead of building on.
            Ok(report) if report.already_aborted => {
                let job_id = crate::jobs::build_job_key(derivation_build);
                self.abort_job(worker_id, job_id, "evaluation aborted".to_owned())
                    .await;
                info!(%derivation_build, %worker_id, "aborting build that started after its evaluation was aborted");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, %derivation_build, "Building update did not reach the graph writer")
            }
        }
    }

    pub async fn handle_build_output(
        &self,
        job_id: &str,
        build_id_str: &str,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        let derivation_build: DerivationBuildId = build_id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid derivation_build: {}", build_id_str))?;

        match self.active_job(job_id).await {
            Some(PendingJob::Build(_)) => {}
            Some(_) => anyhow::bail!("job {} is not a build job", job_id),
            None => {
                warn!(%job_id, "build output for unknown job - ignoring");
                return Ok(());
            }
        }

        self.state
            .graph
            .transition(Transition::BuildOutput {
                shared_build: derivation_build,
                outputs,
                metrics,
                substituted,
            })
            .await
            .map(|_| ())
    }

    pub async fn handle_job_completed(&self, worker_id: &str, job_id: &str) -> Result<()> {
        let worker = worker_id.to_owned();
        let released = self
            .call(|reply| SchedulerMsg::Release {
                worker,
                job_id: job_id.to_owned(),
                reply,
            })
            .await?;
        let Some(job) = released.job else {
            warn!(%job_id, "job_completed for unknown job");
            return Ok(());
        };
        if self.attempt_of(job_id).is_some() && !crate::jobs::is_fetch_only_job(&job) {
            return self
                .on_cluster_member_closed(job_id, MemberReport::Completed { job })
                .await;
        }

        self.settle_completed(job, released.worker_idle).await
    }

    pub(crate) async fn settle_completed(&self, job: PendingJob, worker_idle: bool) -> Result<()> {
        match job {
            PendingJob::Eval(j) => {
                // A fetch-only job in split mode just archived the source. The cached eval
                // follow-up is enqueued under the same `eval:{id}` key that the release above
                // freed.
                if crate::jobs::is_fetch_only(&j.job) {
                    let store_path = EEvaluation::find_by_id(j.evaluation_id)
                        .one(&self.state.worker_db)
                        .await?
                        .and_then(|e| e.flake_source);
                    return match store_path {
                        Some(path) => {
                            let follow_id = crate::jobs::eval_job_key(j.evaluation_id);
                            let mut follow = j.cached_followup(path);
                            if let Some(task) = follow.task_id {
                                follow.history =
                                    self.eval_history.load().for_job(task, &follow.job.steps);
                            }
                            if let Err(e) = self.enqueue_eval_job(follow_id, follow).await {
                                warn!(error = %e, evaluation_id = %j.evaluation_id, "enqueue_eval_job failed for cached follow-up");
                            }
                            info!(evaluation_id = %j.evaluation_id, "fetch complete; enqueued cached eval follow-up");
                            Ok(())
                        }
                        None => {
                            warn!(evaluation_id = %j.evaluation_id, "fetch-only job reported no flake_source; failing eval");
                            self.state
                                .graph
                                .transition(Transition::EvalFailed {
                                    evaluation: j.evaluation_id,
                                    error: "fetch completed but no flake source was archived"
                                        .into(),
                                    kind: BuildFailureKind::Permanent,
                                    missing_paths: Vec::new(),
                                })
                                .await
                                .map(|_| ())
                        }
                    };
                }

                let r = self
                    .state
                    .graph
                    .transition(Transition::EvalStreamCompleted {
                        evaluation: j.evaluation_id,
                    })
                    .await
                    .map(|_| ());
                if worker_idle {
                    self.kick_assigner();
                }

                r
            }
            PendingJob::Build(j) => {
                let report = self
                    .state
                    .graph
                    .transition(Transition::BuildCompleted {
                        shared_build: j.derivation_build,
                    })
                    .await?;
                if let Some(log) = report.substitute_log {
                    let state = Arc::clone(&self.state);
                    self.state.shutdown.spawn(async move {
                        if let Err(e) = crate::log_substitution::substitute_log(
                            state,
                            log.shared_build,
                            log.derivation,
                            log.drv_path,
                        )
                        .await
                        {
                            warn!(error = %e, shared_build = %log.shared_build, "substitute log fetch failed");
                        }
                    });
                }
                if worker_idle || self.has_import_waits() {
                    self.kick_assigner();
                }

                Ok(())
            }
        }
    }

    pub async fn handle_job_failed(
        &self,
        worker_id: &str,
        job_id: &str,
        error: &str,
        kind: BuildFailureKind,
        missing_paths: &[String],
        metrics: Option<BuildMetrics>,
    ) -> Result<()> {
        let worker = worker_id.to_owned();
        let released = self
            .call(|reply| SchedulerMsg::Release {
                worker,
                job_id: job_id.to_owned(),
                reply,
            })
            .await?;
        let Some(job) = released.job else {
            warn!(%job_id, "job_failed for unknown job");
            return Ok(());
        };
        let failure = Failure {
            error: error.to_owned(),
            kind,
            missing_paths: missing_paths.to_vec(),
        };
        if self.attempt_of(job_id).is_some() {
            return self
                .on_cluster_member_closed(job_id, MemberReport::from_failure(job, failure))
                .await;
        }

        self.settle_failed(job, &failure, metrics).await
    }

    pub(crate) async fn settle_failed(
        &self,
        job: PendingJob,
        failure: &Failure,
        metrics: Option<BuildMetrics>,
    ) -> Result<()> {
        match job {
            PendingJob::Eval(j) => {
                let r = self
                    .state
                    .graph
                    .transition(Transition::EvalFailed {
                        evaluation: j.evaluation_id,
                        error: failure.error.clone(),
                        kind: failure.kind,
                        missing_paths: failure.missing_paths.clone(),
                    })
                    .await
                    .map(|_| ());
                self.kick_assigner();
                r
            }
            PendingJob::Build(j) => {
                let r = self
                    .state
                    .graph
                    .transition(Transition::BuildFailed {
                        shared_build: j.derivation_build,
                        error: failure.error.clone(),
                        log_banner: gradient_sources::strip_nix_log_tail(&failure.error),
                        kind: failure.kind,
                        missing_paths: failure.missing_paths.clone(),
                        metrics,
                    })
                    .await
                    .map(|_| ());
                if self.has_import_waits() {
                    self.kick_assigner();
                }

                r
            }
        }
    }
}
