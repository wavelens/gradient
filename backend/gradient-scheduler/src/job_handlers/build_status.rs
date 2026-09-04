/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build status transitions, output recording, and job completion/failure.

use std::sync::Arc;

use anyhow::Result;
use sea_orm::EntityTrait;
use tracing::{info, warn};

use gradient_graph::Transition;
use gradient_types::proto::{BuildFailureKind, BuildMetrics, BuildOutput};
use gradient_types::*;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
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
                anchor: derivation_build,
            })
            .await
        {
            // Backstop for the dispatch/abort race: an anchor dispatched by an
            // in-flight pass just before its evaluation was aborted reports
            // started here, so tell the worker to stop rather than build on.
            Ok(report) if report.already_aborted => {
                let job_id = format!("build:{derivation_build}");
                self.abort_job(worker_id, job_id, "evaluation aborted".to_owned())
                    .await;
                info!(%derivation_build, %worker_id, "aborting build that started after its evaluation was aborted");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, %derivation_build, "Building update did not reach the graph actor")
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
                anchor: derivation_build,
                outputs,
                metrics,
                substituted,
            })
            .await
            .map(|_| ())
    }

    // ── Job completion ────────────────────────────────────────────────────────

    pub async fn handle_job_completed(&self, worker_id: &str, job_id: &str) -> Result<()> {
        let worker = worker_id.to_owned();
        let released = self
            .call(|reply| SchedulerMsg::Release {
                worker,
                job_id: job_id.to_owned(),
                reply,
            })
            .await?;
        let worker_idle = released.worker_idle;
        match released.job {
            Some(PendingJob::Eval(j)) => {
                // Split mode: a fetch-only job just archived the source. Enqueue
                // the cached eval follow-up instead of finalizing - eval has not run.
                if crate::jobs::is_fetch_only(&j.job) {
                    // Reusing the `eval:{id}` job id is safe: remove_active above
                    // already evicted it from the active map.
                    let store_path = EEvaluation::find_by_id(j.evaluation_id)
                        .one(&self.state.worker_db)
                        .await?
                        .and_then(|e| e.flake_source);
                    return match store_path {
                        Some(path) => {
                            let follow_id = format!("eval:{}", j.evaluation_id);
                            if let Err(e) = self
                                .enqueue_eval_job(follow_id, j.cached_followup(path))
                                .await
                            {
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

                // The stream is done, so every endpoint derivation now has a
                // row: the actor settles the still-pending dependency edges and
                // reconciles the closure before the eval moves to Building.
                let r = self
                    .state
                    .graph
                    .transition(Transition::EvalStreamCompleted {
                        evaluation: j.evaluation_id,
                    })
                    .await
                    .map(|_| ());
                if worker_idle {
                    self.kick_dispatch();
                }

                r
            }
            Some(PendingJob::Build(j)) => {
                let report = self
                    .state
                    .graph
                    .transition(Transition::BuildCompleted {
                        anchor: j.derivation_build,
                    })
                    .await?;
                if let Some(log) = report.substitute_log {
                    let state = Arc::clone(&self.state);
                    self.state.shutdown.spawn(async move {
                        if let Err(e) = crate::log_substitution::substitute_log(
                            state,
                            log.anchor,
                            log.derivation,
                            log.drv_path,
                            true,
                        )
                        .await
                        {
                            warn!(error = %e, anchor = %log.anchor, "substitute log fetch failed");
                        }
                    });
                }
                if worker_idle {
                    self.kick_dispatch();
                }

                Ok(())
            }
            None => {
                warn!(%job_id, "job_completed for unknown job");
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
    ) -> Result<()> {
        let worker = worker_id.to_owned();
        let released = self
            .call(|reply| SchedulerMsg::Release {
                worker,
                job_id: job_id.to_owned(),
                reply,
            })
            .await?;
        match released.job {
            Some(PendingJob::Eval(j)) => {
                let r = self
                    .state
                    .graph
                    .transition(Transition::EvalFailed {
                        evaluation: j.evaluation_id,
                        error: error.to_owned(),
                        kind,
                        missing_paths: missing_paths.to_vec(),
                    })
                    .await
                    .map(|_| ());
                // A corrupt-eval-cache heal re-queues the eval; kick dispatch so
                // it re-runs promptly instead of waiting for the next tick.
                self.kick_dispatch();
                r
            }
            Some(PendingJob::Build(j)) => self
                .state
                .graph
                .transition(Transition::BuildFailed {
                    anchor: j.derivation_build,
                    error: error.to_owned(),
                    log_banner: gradient_sources::strip_nix_log_tail(error),
                    kind,
                    missing_paths: missing_paths.to_vec(),
                })
                .await
                .map(|_| ()),
            None => {
                warn!(%job_id, "job_failed for unknown job");
                Ok(())
            }
        }
    }
}
