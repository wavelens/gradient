/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Task executors - one module per job task type.
//!
//! [`JobExecutor`] is the top-level orchestrator that dispatches to the
//! appropriate sub-executor based on the job type received from the server.

pub mod build;
pub mod compress;
pub mod eval;
pub mod fetch;
pub mod log_limit;

use std::sync::Arc;

use anyhow::Result;
use gradient_proto::messages::{BuildJob, FlakeJob, FlakeTask};
use tokio::sync::watch;
use tracing::instrument;

use gradient_types::CachedPathInfo;
use gradient_proto::messages::QueryMode;

use crate::nix::gcroots::{GcRootHandle, GcRootKeeper};
use crate::nix::store::LocalNixStore;
use crate::proto::{credentials::CredentialStore, job::JobUpdater, nar};
use gradient_proto::messages::CachedPath;

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

/// Push the runtime closure of every `.drv` produced during eval to the
/// gradient cache.
///
/// Walking the closure (rather than just the `.drv` files themselves) is what
/// keeps downstream build workers from racing the cache: a `.drv` references
/// every `input_source` (e.g. `builtins.path`, `lib.cleanSource` outputs that
/// landed in the eval worker's local store) and every transitive `.drv`. If
/// we only pushed the `.drv` files, a downstream worker's `prefetch_inputs`
/// could query the cache, find the `.drv` cached, then fail on import because
/// a referenced source is still local-only. Walking the closure here ensures
/// every store path needed to interpret a produced `.drv` is in the cache
/// before this eval job's `JobCompleted` reaches the server.
///
/// Errors during closure expansion or individual NAR pushes are logged but
/// not fatal - operators see them in the worker log, and downstream builds
/// surface the missing path through their existing prefetch error path.
async fn push_drv_closure(drv_paths: &[String], updater: &mut JobUpdater, store: &LocalNixStore) {
    if drv_paths.is_empty() {
        return;
    }

    let closure = store.collect_runtime_closure(drv_paths).await;
    if closure.is_empty() {
        return;
    }
    tracing::debug!(
        seeds = drv_paths.len(),
        closure = closure.len(),
        "pushing eval closure to cache"
    );

    let paths: Vec<String> = closure.into_iter().collect();
    let cache_entries = query_fetched_paths(updater, paths).await;
    for cp in &cache_entries {
        push_one_fetched_nar(updater, cp, store).await;
    }
}

/// Upload one fetched input path's NAR to the cache - either via a presigned
/// PUT URL (S3) or via the chunked WS `NarPush` fallback (local storage).
///
/// Errors are logged and swallowed; a failed push for a source path is not
/// fatal - the build proceeds and fails cleanly if the daemon truly needs it.
async fn push_one_fetched_nar(updater: &mut JobUpdater, cp: &CachedPath, store: &LocalNixStore) {
    match cp.as_info() {
        CachedPathInfo::Cached { .. } => {
            tracing::debug!(store_path = %cp.path, "skipping NAR push - already cached");
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: Some(url),
        } => {
            tracing::debug!(store_path = %path, "uploading NAR via presigned PUT URL");
            if let Err(e) = nar::upload_presigned(
                &updater.job_id,
                path,
                url,
                "PUT",
                &[],
                &updater.writer,
                Some(store),
            )
            .await
            {
                tracing::warn!(store_path = %path, error = %e, "presigned NAR upload failed; continuing");
            }
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: None,
        } => {
            if let Err(e) =
                nar::push_direct(&updater.job_id, path, &updater.writer, &updater.nar_recv, Some(store))
                    .await
            {
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
    pub(crate) gcroots: GcRootKeeper,
    pub(crate) binpath_nix: String,
    pub(crate) binpath_ssh: String,
    pub(crate) build_metrics: bool,
    pub(crate) build_cgroup_root: String,
    pub(crate) build_cgroup_state_dir: String,
    pub(crate) log_limits: crate::executor::log_limit::LogRateLimits,
    pub(crate) log_fetch_from_store: bool,
}

impl JobExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: LocalNixStore,
        evaluator: WorkerEvaluator,
        gcroots: GcRootKeeper,
        binpath_nix: String,
        binpath_ssh: String,
        build_metrics: bool,
        build_cgroup_root: String,
        build_cgroup_state_dir: String,
        log_limits: crate::executor::log_limit::LogRateLimits,
        log_fetch_from_store: bool,
    ) -> Self {
        Self {
            store: Arc::new(store),
            evaluator: Arc::new(evaluator),
            gcroots,
            binpath_nix,
            binpath_ssh,
            build_metrics,
            build_cgroup_root,
            build_cgroup_state_dir,
            log_limits,
            log_fetch_from_store,
        }
    }

    /// Gracefully shut every idle eval-worker subprocess down so libnix's
    /// atexit handlers run (flush eval-cache SQLite, drop temp GC roots)
    /// instead of being SIGKILL'd by `kill_on_drop` when the runtime tears
    /// down on signal.
    pub async fn shutdown(&self) {
        self.evaluator.shutdown().await;
    }

    /// Execute a `FlakeJob` (fetch → eval-flake → eval-derivations).
    ///
    /// When `FetchFlake` and eval tasks are in the same job, the local clone
    /// path from the fetch is reused for evaluation - the repo is cloned
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
                    let outcome = fetch::fetch_repository(
                        &job,
                        updater as &mut dyn gradient_proto::traits::JobReporter,
                        credentials,
                        &self.binpath_nix,
                        &self.binpath_ssh,
                        abort.clone(),
                    )
                    .await?;

                    let cache_entries =
                        query_fetched_paths(updater, outcome.archived_paths.clone()).await;
                    for cp in &cache_entries {
                        push_one_fetched_nar(updater, cp, &self.store).await;
                    }

                    updater.report_fetch_result(outcome.flake_source.clone())?;
                    local_flake_path = Some(outcome.local_flake_path);
                }
                FlakeTask::EvaluateFlake => eval::evaluate_flake(&job, updater).await?,
                FlakeTask::EvaluateDerivations => {
                    // A `Cached` source was archived to a *different* worker's
                    // store and pushed to the cache; substitute it locally
                    // before eval, since nix won't pull a `path:` flake ref
                    // from a binary cache.
                    if local_flake_path.is_none()
                        && let Some(src) = eval::required_local_source(&job.source)
                    {
                        crate::proto::nar_import::ensure_path(&self.store, src, updater).await?;
                    }

                    let produced_drvs = eval::evaluate_derivations(
                        &self.evaluator,
                        &job,
                        local_flake_path.as_deref(),
                        updater,
                        &mut abort.clone(),
                    )
                    .await?;

                    // Push the full runtime closure of every produced
                    // `.drv` to the cache so substituters can fetch
                    // `<drv-hash>.narinfo` *and* downstream builds find
                    // every `input_source` they need on prefetch. Runs
                    // after eval so the server already has the derivation
                    // rows; ordering between the NAR and the row doesn't
                    // matter (the server keys cached_path by hash). The
                    // server computes narinfo signatures from the uploaded
                    // metadata.
                    push_drv_closure(&produced_drvs, updater, &self.store).await;
                }
            }
        }
        Ok(())
    }

    /// Execute a `BuildJob` (builds → compress → push).
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
        _credentials: &CredentialStore,
        mut abort: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut all_output_paths: Vec<String> = Vec::new();
        let mut gc_handles: Vec<GcRootHandle> = Vec::new();
        for (index, build_task) in job.builds.iter().enumerate() {
            check_abort(&mut abort)?;
            // Move the build to `Building` on the server *before* anything
            // that can fail. The state machine only allows
            // `Building → Failed`; if we let prefetch (or anything before
            // `report_building`) bubble up an error first, the eventual
            // `JobFailed` would arrive at the server while the build is
            // still `Queued`, the transition would be rejected, and the UI
            // would show the build hanging in `Queued` forever.
            updater.report_building(build_task.build_id.clone())?;

            // Pin the .drv as an indirect GC root before prefetching its
            // inputs. Nix's reachability walks .drv references
            // (input_drvs + input_sources), so one root covers the entire
            // build-time closure.
            gc_handles.push(self.gcroots.add(&build_task.drv_path).await);

            if build_task.external_cached {
                // Substitute attempt: the output was reported cache-available
                // during eval, so this build was dispatched arch-agnostically
                // (`builtin`). Pulling it through is the whole job - there is no
                // safe local-build fallback (this worker may be the wrong arch).
                // On a miss, fail with `SubstituteUnavailable`; the scheduler
                // re-dispatches or escalates to a real arch-bound build.
                let outputs = crate::proto::nar_import::fetch_external_cached_outputs(
                    &self.store,
                    build_task,
                    updater,
                )
                .await
                .map_err(|e| {
                    tracing::warn!(
                        build_id = %build_task.build_id,
                        error = %e,
                        "external_cached fetch missed; reporting SubstituteUnavailable"
                    );
                    crate::executor::build::BuildError::substitute_unavailable(e)
                })?;

                let mut reported = Vec::with_capacity(outputs.len());
                for (name, path) in &outputs {
                    let hash = gradient_sources::get_hash_from_path(path.clone())
                        .map(|(h, _)| h)
                        .unwrap_or_default();
                    let products = build::load_products(path).await;
                    reported.push(gradient_proto::messages::BuildOutput {
                        name: name.clone(),
                        store_path: path.clone(),
                        hash,
                        nar_size: None,
                        nar_hash: None,
                        products,
                    });
                }

                updater.report_build_output(build_task.build_id.clone(), reported, None, false)?;
                for (_, p) in &outputs {
                    gc_handles.push(self.gcroots.add(p).await);
                }

                all_output_paths.extend(outputs.into_iter().map(|(_, p)| p));
                continue;
            }

            // Import cache-resident inputs the daemon will need. A hard
            // local-store error (e.g. `store.has_path` failing) aborts the
            // build - we can't safely proceed without knowing what's already
            // in the store. Other prefetch errors (CacheQuery transport,
            // individual NAR downloads) are logged inside `prefetch_inputs`
            // and don't reach here as `Err`.
            crate::proto::nar_import::prefetch_inputs(&self.store, build_task, updater)
                .await
                .map_err(|e| {
                    tracing::error!(
                        build_id = %build_task.build_id,
                        error = %e,
                        "input prefetch failed; aborting build"
                    );
                    crate::executor::build::BuildError::transient(e)
                })?;
            let outputs = build::build_derivation(
                &self.store,
                build_task,
                index as u32,
                updater,
                &mut abort,
                self.build_metrics,
                &self.build_cgroup_root,
                &self.build_cgroup_state_dir,
                self.log_limits,
                self.log_fetch_from_store,
            )
            .await?;
            for o in &outputs {
                gc_handles.push(self.gcroots.add(&o.store_path).await);
            }
            all_output_paths.extend(outputs.into_iter().map(|o| o.store_path));
        }

        // Always compress+push every realised output. The worker is the sole
        // producer of compressed NARs; the server stores them and computes
        // narinfo signatures from the uploaded metadata. Honours `abort`
        // between paths so an `AbortJob` from the server (e.g. session NAR
        // buffer exceeded) terminates the upload loop and surfaces as a
        // `JobFailed`.
        compress::compress_and_push_paths(&self.store, &all_output_paths, updater, &mut abort)
            .await
            .map_err(crate::executor::build::BuildError::transient)?;

        // Release every indirect GC root for this job; symlinks are removed
        // and the daemon's next GC walk is free to delete unreachable paths.
        drop(gc_handles);
        Ok(())
    }
}

/// Propagate a server-side `AbortJob` as an error so the surrounding job
/// resolves to `JobFailed` instead of `JobCompleted`.
pub(crate) fn check_abort(abort: &mut watch::Receiver<bool>) -> Result<()> {
    if *abort.borrow() {
        anyhow::bail!("job aborted by server");
    }
    Ok(())
}
