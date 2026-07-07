/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build status transitions, output recording, and job completion/failure.

use anyhow::Result;
use gradient_entity::build::BuildStatus;
use sea_orm::EntityTrait;
use tracing::{error, info, warn};

use gradient_types::proto::{BuildFailureKind, BuildMetrics, BuildOutput};
use gradient_types::*;

use crate::Scheduler;
use crate::jobs::PendingJob;
use crate::{build, eval};

impl Scheduler {
    pub async fn handle_build_status_update(&self, build_id_str: &str, worker_id: &str) {
        let derivation_build = match build_id_str.parse::<DerivationBuildId>() {
            Ok(id) => id,
            Err(_) => {
                warn!(%build_id_str, "invalid derivation_build in Building update");
                return;
            }
        };

        match EDerivationBuild::find_by_id(derivation_build)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(anchor)) => {
                // Backstop for the dispatch/abort race: an anchor dispatched by an
                // in-flight pass just before its evaluation was aborted reports
                // started here. Its status is already Aborted, so tell the worker
                // to stop instead of letting it build to completion.
                if anchor.status == BuildStatus::Aborted {
                    let job_id = format!("build:{derivation_build}");
                    self.worker_pool.read().await.send_abort(
                        worker_id,
                        job_id,
                        "evaluation aborted".to_owned(),
                    );
                    info!(%derivation_build, %worker_id, "aborting build that started after its evaluation was aborted");
                    return;
                }

                gradient_db::update_derivation_build_status(
                    &self.state.db(),
                    anchor,
                    BuildStatus::Building,
                )
                .await;
            }
            Ok(None) => warn!(%derivation_build, "anchor not found for Building status update"),
            Err(e) => {
                warn!(error = %e, %derivation_build, "failed to fetch anchor for status update")
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

        let job = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j.clone(),
                Some(_) => anyhow::bail!("job {} is not a build job", job_id),
                None => {
                    warn!(%job_id, "build output for unknown job - ignoring");
                    return Ok(());
                }
            }
        };
        build::handle_build_output(
            &self.state,
            &job,
            derivation_build,
            outputs,
            metrics,
            substituted,
        )
        .await
    }

    // ── Job completion ────────────────────────────────────────────────────────

    pub async fn handle_job_completed(&self, worker_id: &str, job_id: &str) -> Result<()> {
        let worker_idle = self
            .worker_pool
            .write()
            .await
            .release_job(worker_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
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
                            self.enqueue_eval_job(follow_id, j.cached_followup(path))
                                .await;
                            info!(evaluation_id = %j.evaluation_id, "fetch complete; enqueued cached eval follow-up");
                            Ok(())
                        }
                        None => {
                            warn!(evaluation_id = %j.evaluation_id, "fetch-only job reported no flake_source; failing eval");
                            eval::handle_eval_job_failed(
                                &self.state,
                                j.evaluation_id,
                                "fetch completed but no flake source was archived",
                            )
                            .await
                        }
                    };
                }

                // The stream is done, so every endpoint derivation now has a
                // row: flush the dependency edges still pending after the
                // incremental per-batch flushes so the graph is complete for
                // promotion + dispatch.
                let edges = self
                    .eval_edges
                    .write()
                    .await
                    .remove(&j.evaluation_id)
                    .unwrap_or_default()
                    .into_pending();
                if let Err(e) = eval::flush_deferred_deps(&self.state, j.evaluation_id, edges).await
                {
                    error!(error = %e, evaluation_id = %j.evaluation_id, "flush_deferred_deps failed");
                }
                let r = eval::handle_eval_job_completed(&self.state, j.evaluation_id).await;
                if worker_idle {
                    self.kick_dispatch();
                }

                r
            }
            Some(PendingJob::Build(j)) => {
                let r = build::handle_build_job_completed(&self.state, j.derivation_build).await;
                if worker_idle {
                    self.kick_dispatch();
                }

                r
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
        self.worker_pool
            .write()
            .await
            .release_job(worker_id, job_id);
        let job = self.job_tracker.write().await.remove_active(job_id);
        match job {
            Some(PendingJob::Eval(j)) => {
                self.eval_edges.write().await.remove(&j.evaluation_id);
                eval::handle_eval_job_failed(&self.state, j.evaluation_id, error).await
            }
            Some(PendingJob::Build(j)) => {
                build::handle_build_job_failed(
                    &self.state,
                    j.derivation_build,
                    error,
                    kind,
                    missing_paths,
                )
                .await
            }
            None => {
                warn!(%job_id, "job_failed for unknown job");
                Ok(())
            }
        }
    }
}
