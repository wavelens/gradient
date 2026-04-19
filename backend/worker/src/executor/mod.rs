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

use gradient_core::types::CachedPathInfo;
use proto::messages::QueryMode;

use crate::nix::store::LocalNixStore;
use crate::proto::{credentials::CredentialStore, job::JobUpdater, nar};
use proto::messages::CachedPath;

pub use eval::WorkerEvaluator;

// ── Fetch helpers ─────────────────────────────────────────────────────────────

/// Query the server for which fetched input paths are already cached, falling
/// back to "treat everything as uncached" when the query fails.
async fn query_fetched_paths(updater: &mut JobUpdater, all_paths: Vec<String>) -> Vec<CachedPath> {
    if all_paths.is_empty() {
        return vec![];
    }
    match updater
        .query_cache(all_paths.clone(), QueryMode::Push)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "CacheQuery failed; will attempt direct push for all paths");
            all_paths
                .iter()
                .map(|p| CachedPath {
                    path: p.clone(),
                    cached: false,
                    file_size: None,
                    nar_size: None,
                    url: None,
                    nar_hash: None,
                    references: None,
                    signatures: None,
                    deriver: None,
                    ca: None,
                })
                .collect()
        }
    }
}

/// Upload one fetched input path's NAR to the cache — either via a presigned
/// PUT URL (S3) or via the chunked WS `NarPush` fallback (local storage).
///
/// Errors are logged and swallowed; a failed push for a source path is not
/// fatal — the build proceeds and fails cleanly if the daemon truly needs it.
async fn push_one_fetched_nar(updater: &mut JobUpdater, cp: &CachedPath) {
    match cp.as_info() {
        CachedPathInfo::Cached { .. } => {
            tracing::debug!(store_path = %cp.path, "skipping NAR push — already cached");
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: Some(url),
        } => {
            tracing::debug!(store_path = %path, "uploading NAR via presigned PUT URL");
            if let Err(e) =
                nar::upload_presigned(&updater.job_id, path, url, "PUT", &[], &updater.writer, None).await
            {
                tracing::warn!(store_path = %path, error = %e, "presigned NAR upload failed; continuing");
            }
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: None,
        } => {
            tracing::info!(
                store_path = %path,
                "server returned no presigned URL — falling back to direct NarPush \
                 (expected for local-mode caches; check server logs for \
                 'presigned PUT URL generation failed' if you have S3 configured)"
            );
            if let Err(e) = nar::push_direct(&updater.job_id, path, &updater.writer, None).await {
                tracing::warn!(store_path = %path, error = %e, "failed to push NAR for fetched input; continuing");
            }
        }
    }
}

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

                    let all_paths: Vec<String> = fetched_inputs
                        .iter()
                        .map(|fi| fi.store_path.clone())
                        .collect();
                    let cache_entries = query_fetched_paths(updater, all_paths).await;
                    for cp in &cache_entries {
                        push_one_fetched_nar(updater, cp).await;
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
                        &mut abort.clone(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Execute a `BuildJob` (builds → compress → sign).
    ///
    /// Before each derivation is built, we prefetch any of its input store
    /// paths that aren't in the local store from the server's cache (via
    /// `CacheQuery {Pull}` + presigned URL download or `NarRequest`). Without
    /// this, the daemon would fail with "1 dependency failed" the moment it
    /// tries to build a derivation whose inputs were produced on a different
    /// worker.
    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
    ) -> Result<()> {
        for (index, build_task) in job.builds.iter().enumerate() {
            // Move the build to `Building` on the server *before* anything
            // that can fail. The state machine only allows
            // `Building → Failed`; if we let prefetch (or anything before
            // `report_building`) bubble up an error first, the eventual
            // `JobFailed` would arrive at the server while the build is
            // still `Queued`, the transition would be rejected, and the UI
            // would show the build hanging in `Queued` forever.
            updater.report_building(build_task.build_id.clone())?;

            // Best-effort prefetch: import any cache-resident inputs the
            // daemon will need. A failure here doesn't abort the build —
            // the daemon will error out cleanly if a critical input is
            // truly missing, and that error is more diagnosable than the
            // generic "prefetch failed" we'd raise here.
            if let Err(e) =
                crate::proto::nar_import::prefetch_inputs(&self.store, build_task, updater).await
            {
                tracing::warn!(
                    build_id = %build_task.build_id,
                    error = %e,
                    "input prefetch failed; build will proceed and fail fast if any input is unavailable"
                );
            }
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
