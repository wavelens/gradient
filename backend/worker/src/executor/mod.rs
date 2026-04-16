/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Task executors — one module per job task type.
//!
//! [`JobExecutor`] is the top-level orchestrator that dispatches to the
//! appropriate sub-executor based on the job type received from the server.

pub mod build;
pub mod compress;
pub mod eval;
pub mod fetch;
pub mod sign;

use std::sync::Arc;

use anyhow::Result;
use proto::messages::{BuildJob, FlakeJob, FlakeTask};
use tokio::sync::watch;
use tracing::instrument;

use proto::messages::QueryMode;

use crate::nix::store::LocalNixStore;
use crate::proto::{credentials::CredentialStore, job::JobUpdater, nar};

pub use eval::WorkerEvaluator;

/// Executes jobs dispatched by the server.
///
/// Each method corresponds to one `Job` variant from the proto spec.
/// Results and status updates are sent back through [`JobUpdater`].
///
/// Arc-wraps the store and evaluator so the executor can be cheaply cloned
/// and moved into spawned job tasks.
#[derive(Clone)]
pub struct JobExecutor {
    pub(crate) store: Arc<LocalNixStore>,
    pub(crate) evaluator: Arc<WorkerEvaluator>,
    pub(crate) binpath_nix: String,
    pub(crate) binpath_ssh: String,
}

impl JobExecutor {
    pub fn new(
        store: LocalNixStore,
        evaluator: WorkerEvaluator,
        binpath_nix: String,
        binpath_ssh: String,
    ) -> Self {
        Self {
            store: Arc::new(store),
            evaluator: Arc::new(evaluator),
            binpath_nix,
            binpath_ssh,
        }
    }

    /// Execute a `FlakeJob` (fetch → eval-flake → eval-derivations).
    ///
    /// When `FetchFlake` and eval tasks are in the same job, the local clone
    /// path from the fetch is reused for evaluation — the repo is cloned
    /// exactly once.
    #[instrument(skip_all, fields(tasks = ?job.tasks))]
    pub async fn execute_flake_job(
        &self,
        job: FlakeJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: watch::Receiver<bool>,
    ) -> Result<()> {
        // If FetchFlake runs, it stores the local checkout path here so
        // subsequent eval tasks use it instead of the remote URL.
        let mut local_flake_path: Option<String> = None;

        for task in &job.tasks {
            match task {
                FlakeTask::FetchFlake => {
                    let (path, fetched_inputs) = fetch::fetch_repository(
                        &job,
                        updater as &mut dyn proto::traits::JobReporter,
                        credentials,
                        &self.binpath_nix,
                        &self.binpath_ssh,
                        abort.clone(),
                    )
                    .await?;

                    let all_paths: Vec<String> =
                        fetched_inputs.iter().map(|fi| fi.store_path.clone()).collect();

                    let cache_entries = if all_paths.is_empty() {
                        vec![]
                    } else {
                        match updater.query_cache(all_paths.clone(), QueryMode::Push).await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(error = %e, "CacheQuery failed; will attempt direct push for all paths");
                                // Treat all paths as uncached with no URL.
                                all_paths.iter().map(|p| proto::messages::CachedPath {
                                    path: p.clone(),
                                    cached: false,
                                    file_size: None,
                                    nar_size: None,
                                    url: None,
                                }).collect()
                            }
                        }
                    };

                    for cp in &cache_entries {
                        if cp.cached {
                            tracing::debug!(store_path = %cp.path, "skipping NAR push — already cached");
                            continue;
                        }
                        if let Some(url) = &cp.url {
                            // S3 presigned upload.
                            if let Err(e) = nar::upload_presigned(
                                &updater.job_id,
                                &cp.path,
                                url,
                                "PUT",
                                &[],
                                &updater.writer,
                            )
                            .await
                            {
                                tracing::warn!(
                                    store_path = %cp.path,
                                    error = %e,
                                    "presigned NAR upload failed; continuing"
                                );
                            }
                        } else if let Err(e) =
                            nar::push_direct(&updater.job_id, &cp.path, &updater.writer).await
                        {
                            tracing::warn!(
                                store_path = %cp.path,
                                error = %e,
                                "failed to push NAR for fetched input; continuing"
                            );
                        }
                    }
                    updater.report_fetch_result(fetched_inputs)?;
                    local_flake_path = Some(path);
                }
                FlakeTask::EvaluateFlake => eval::evaluate_flake(&job, updater).await?,
                FlakeTask::EvaluateDerivations => {
                    eval::evaluate_derivations(
                        &self.evaluator,
                        &job,
                        local_flake_path.as_deref(),
                        updater,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Execute a `BuildJob` (builds → compress → sign).
    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
    ) -> Result<()> {
        for (index, build_task) in job.builds.iter().enumerate() {
            build::build_derivation(&self.store, build_task, index as u32, updater).await?;
        }

        if let Some(compress_task) = &job.compress {
            compress::compress_outputs(&self.store, compress_task, updater).await?;
        }

        if let Some(sign_task) = &job.sign {
            sign::sign_outputs(&self.store, credentials, sign_task, updater).await?;
        }

        Ok(())
    }
}
