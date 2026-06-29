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
                    file_hash: None,
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
/// The daemon's reference walk is unreliable for a `.drv`'s `inputSrcs`, so the
/// sources are additionally discovered by parsing each `.drv`
/// ([`drv_input_sources`]), mirroring the build-side prefetch. They have no
/// producing derivation, so a missed source cannot self-heal - the build just
/// fails `InputsUnavailable` forever.
///
/// A failed closure upload fails the evaluation (propagated to the caller),
/// so a downstream build never starts against a source the cache is missing.
pub(crate) async fn push_drv_closure(
    drv_paths: &[String],
    updater: &mut JobUpdater,
    store: &LocalNixStore,
) -> Result<()> {
    if drv_paths.is_empty() {
        return Ok(());
    }

    let mut closure = store.collect_runtime_closure(drv_paths).await;

    // The daemon's reference walk drops a `.drv`'s `inputSrcs`, so discover them
    // authoritatively by parsing each `.drv` - mirroring the build-side prefetch
    // (`InputPrefetcher::enumerate_inputs`). Parse EVERY `.drv` in the closure, not
    // just the seeds: a pruned/transitive node's input source (a producerless
    // config file like `etc-machine-id`, or a vendored `cargo-src-*`) is otherwise
    // never pushed, and a later rebuild of that node fails `InputsUnavailable`
    // forever on a source that has no producer and only the eval worker holds.
    let drv_members: Vec<String> = closure
        .iter()
        .filter(|p| p.ends_with(".drv"))
        .cloned()
        .collect();
    closure.extend(drv_input_sources(&drv_members).await);

    if closure.is_empty() {
        return Ok(());
    }
    tracing::debug!(
        seeds = drv_paths.len(),
        closure = closure.len(),
        "pushing eval closure to cache"
    );

    let paths: Vec<String> = closure.into_iter().collect();
    let cache_entries = query_fetched_paths(updater, paths).await;
    for cp in &cache_entries {
        upload_one_nar(updater, cp, store).await?;
    }

    Ok(())
}

/// The `inputSrcs` declared by each `.drv`, read by parsing the file directly
/// rather than via the daemon's reference walk (which is unreliable for a
/// `.drv`'s sources). Mirrors the build-side prefetch so every source a build
/// worker will demand is pushed by the eval that produced it. A `.drv` that
/// cannot be read or parsed is skipped (logged), not fatal - the daemon closure
/// still covers it.
async fn drv_input_sources(drv_paths: &[String]) -> std::collections::HashSet<String> {
    use crate::nix::store::canonicalize_store_path;
    use futures::stream::{self, StreamExt as _};

    const DRV_READ_CONCURRENCY: usize = 64;

    stream::iter(drv_paths.iter().cloned())
        .map(|drv_path| async move {
            let full = canonicalize_store_path(&drv_path);
            match tokio::fs::read(&full).await {
                Ok(bytes) => match gradient_db::parse_drv(&bytes) {
                    Ok(drv) => drv.input_sources,
                    Err(e) => {
                        tracing::warn!(drv = %drv_path, error = %e, "push: cannot parse .drv for input sources");
                        Vec::new()
                    }
                },
                Err(e) => {
                    tracing::warn!(drv = %drv_path, error = %e, "push: cannot read .drv for input sources");
                    Vec::new()
                }
            }
        })
        .buffer_unordered(DRV_READ_CONCURRENCY)
        .concat()
        .await
        .into_iter()
        .collect()
}

/// Classify a failed `external_cached` relay. Only a genuine "not on any
/// upstream" miss ([`SubstituteNotOnUpstream`]) is reported as
/// `SubstituteUnavailable` (which counts toward the scheduler's miss-escalation
/// threshold). A transient timeout/transport failure (Pull RPC, NAR download, or
/// the presigned PUT into our own store) is `Transient` so it retries as a
/// substitute instead of escalating an otherwise-substitutable build into a
/// from-scratch one whose `.drv` may never have been pushed.
fn classify_substitute_failure(
    build_id: &str,
    e: anyhow::Error,
) -> crate::executor::build::BuildError {
    use crate::executor::build::BuildError;
    use crate::proto::nar_import::SubstituteNotOnUpstream;

    if e.chain().any(|c| c.is::<SubstituteNotOnUpstream>()) {
        tracing::warn!(%build_id, error = %e, "external_cached relay: output on no upstream; SubstituteUnavailable");
        BuildError::substitute_unavailable(e)
    } else {
        tracing::warn!(%build_id, error = %e, "external_cached relay failed transiently; retrying without escalating");
        BuildError::transient(e)
    }
}

/// Upload one path's NAR using the method the server advertised in its
/// `CacheQuery {Push}` response: a presigned S3 PUT straight to object storage
/// when available, else the chunked WS `NarPush` fallback (local stores).
/// Already-cached paths are skipped. Errors are returned so the caller decides
/// whether they are fatal.
pub(crate) async fn upload_one_nar(
    updater: &JobUpdater,
    cp: &CachedPath,
    store: &LocalNixStore,
) -> Result<()> {
    match cp.as_info() {
        CachedPathInfo::Cached { .. } => {
            tracing::debug!(store_path = %cp.path, "skipping NAR upload - already cached");
            Ok(())
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: Some(url),
        } => {
            tracing::debug!(store_path = %path, "uploading NAR via presigned PUT URL");
            nar::upload_presigned(
                &updater.job_id,
                path,
                url,
                "PUT",
                &[],
                &updater.writer,
                Some(store),
            )
            .await
        }
        CachedPathInfo::Uncached {
            path,
            upload_url: None,
        } => {
            nar::push_direct(&updater.job_id, path, &updater.writer, &updater.nar_recv, Some(store)).await
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
                        upload_one_nar(updater, cp, &self.store).await?;
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

                    // Each batch's `.drv` runtime closure (input_sources + .drvs,
                    // for narinfo substitution and downstream-build prefetch) is
                    // pushed to the cache inside the walk, before that batch's
                    // `report_eval_result` - so #392's mid-eval build dispatch
                    // never races the source upload. The server keys cached_path
                    // by hash, so NAR/row ordering is irrelevant.
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

            if build_task.external_cached {
                // Substitute attempt: the output is on an upstream cache (flagged
                // at eval). Relay it as a pure NAR copy - download the output NAR
                // from upstream, store it verbatim (recompress only when its zstd
                // window is below our level-6 2 MiB threshold), push it to our
                // cache - without importing into the nix store or fetching the closure (the closure
                // is mirrored by each member's own anchor). There is no local-build
                // fallback (this worker may be the wrong arch); on a miss fail with
                // `SubstituteUnavailable` and let the scheduler re-dispatch/escalate.
                let outputs = crate::proto::nar_import::relay_external_cached_outputs(
                    build_task,
                    updater,
                )
                .await
                .map_err(|e| classify_substitute_failure(&build_task.build_id, e))?;

                let reported: Vec<gradient_proto::messages::BuildOutput> = outputs
                    .iter()
                    .map(|(name, path)| gradient_proto::messages::BuildOutput {
                        name: name.clone(),
                        store_path: path.clone(),
                        hash: gradient_sources::get_hash_from_path(path.clone())
                            .map(|(h, _)| h)
                            .unwrap_or_default(),
                        nar_size: None,
                        nar_hash: None,
                        products: Vec::new(),
                    })
                    .collect();

                // The relay already pushed each NAR (NarUploaded), and nothing
                // landed in the local store, so no GC roots and no post-loop
                // compress_and_push for these outputs.
                updater.report_build_output(build_task.build_id.clone(), reported, None, false)?;
                continue;
            }

            // Pin the .drv as an indirect GC root before prefetching its
            // inputs. Nix's reachability walks .drv references
            // (input_drvs + input_sources), so one root covers the entire
            // build-time closure. external_cached substitutions never fetch the
            // .drv, so this only applies to real builds.
            gc_handles.push(self.gcroots.add(&build_task.drv_path).await);

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
                    // A "required inputs not in cache" miss is terminal and
                    // self-healing server-side: forward the paths so the server
                    // demotes them and re-queues their producers. A cached NAR
                    // that fails integrity (its bytes don't match the recorded
                    // nar_hash, e.g. a non-reproducible local build desynced from
                    // upstream-substitute metadata) is the same class: report the
                    // path so the server demotes the corrupt object and rebuilds
                    // it. Every other prefetch error is infrastructure-transient.
                    if let Some(mi) = e.downcast_ref::<crate::proto::nar_import::MissingInputs>() {
                        crate::executor::build::BuildError::inputs_unavailable(mi.0.clone(), e)
                    } else if let Some(corrupt) = e
                        .chain()
                        .find_map(|s| s.downcast_ref::<crate::proto::nar_import::CorruptCachedNar>())
                    {
                        crate::executor::build::BuildError::inputs_unavailable(
                            vec![corrupt.0.clone()],
                            e,
                        )
                    } else {
                        crate::executor::build::BuildError::transient(e)
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a genuine "not on any upstream" miss escalates; a transient relay
    /// timeout (Pull RPC / NAR download / presigned PUT) retries as a substitute
    /// instead of counting toward miss-escalation - two transient timeouts must
    /// not turn a substitutable build into a from-scratch one.
    #[test]
    fn substitute_failure_classification() {
        use crate::proto::nar_import::SubstituteNotOnUpstream;
        use gradient_proto::messages::BuildFailureKind;

        let genuine = classify_substitute_failure(
            "b",
            anyhow::Error::new(SubstituteNotOnUpstream("/nix/store/p".into())),
        );
        assert!(matches!(genuine.kind, BuildFailureKind::SubstituteUnavailable));

        // wrapped in context is still recognized via the error chain
        let wrapped = classify_substitute_failure(
            "b",
            anyhow::Error::new(SubstituteNotOnUpstream("/nix/store/p".into())).context("relay"),
        );
        assert!(matches!(wrapped.kind, BuildFailureKind::SubstituteUnavailable));

        let timeout = classify_substitute_failure("b", anyhow::anyhow!("operation timed out"));
        assert!(matches!(timeout.kind, BuildFailureKind::Transient));
    }

    /// Regression: a `.drv`'s `inputSrcs` (e.g. `builtins.toFile` configs like
    /// `grub-config.xml`) must be discovered by parsing the `.drv`, not via the
    /// daemon reference walk - the latter drops them, so the eval never pushed
    /// them and the build failed `InputsUnavailable` with no self-heal.
    #[tokio::test]
    async fn drv_input_sources_parses_inputsrcs_not_via_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let drv = dir.path().join("nixos-system.drv");
        tokio::fs::write(
            &drv,
            br#"Derive([("out","/nix/store/abc-out","","")],[("/nix/store/dep.drv",["out"])],["/nix/store/s1-grub-config.xml","/nix/store/s2-grub-config.xml"],"x86_64-linux","/nix/store/bash",["-e"],[("name","nixos-system")])"#,
        )
        .await
        .unwrap();

        let srcs = drv_input_sources(&[drv.to_string_lossy().into_owned()]).await;

        assert!(srcs.contains("/nix/store/s1-grub-config.xml"));
        assert!(srcs.contains("/nix/store/s2-grub-config.xml"));
        assert!(
            !srcs.contains("/nix/store/dep.drv"),
            "an input derivation is not an input source"
        );
    }

    /// An unreadable `.drv` is skipped, not fatal - the daemon closure covers it.
    #[tokio::test]
    async fn drv_input_sources_skips_unreadable_drv() {
        let srcs = drv_input_sources(&["/nix/store/does-not-exist.drv".to_string()]).await;
        assert!(srcs.is_empty());
    }
}
