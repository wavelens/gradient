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
mod build_metrics;
pub mod compress;
mod derivation;
mod download;
pub mod eval;
pub(crate) mod failure;
pub mod fetch;
pub mod log_limit;
mod substitute;
pub mod timeline;

use std::sync::Arc;

use anyhow::Result;
use gradient_proto::messages::{
    BuildJob, BuildOutput, BuildSpec, BuildSpecKind, FlakeJob, FlakeStep,
};
use tokio::sync::watch;
use tracing::instrument;

use gradient_proto::messages::JobPhase;
use gradient_types::CachedPathInfo;

use crate::nix::gcroots::{GcRootHandle, GcRootKeeper};
use crate::nix::store::LocalNixStore;
use crate::proto::{credentials::CredentialStore, job::JobUpdater, nar};
use gradient_proto::messages::CachedPath;
use gradient_proto::traits::WorkerStore;

pub use eval::WorkerEvaluator;

// ── Fetch helpers ─────────────────────────────────────────────────────────────

/// Query the server for which fetched input paths are already cached, falling
/// back to "treat everything as uncached" when the query fails.
async fn query_fetched_paths(
    updater: &mut JobUpdater,
    all_paths: Vec<String>,
    sizes: Vec<Option<u64>>,
) -> Vec<CachedPath> {
    if all_paths.is_empty() {
        return vec![];
    }
    match updater.query_push(all_paths.clone(), sizes).await {
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
                    multipart: None,
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

    let mut guard = updater.phase(JobPhase::DrvClosurePush);
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
    guard.record(paths.len() as u32, 0);
    let sizes = vec![None; paths.len()];
    let cache_entries = query_fetched_paths(updater, paths, sizes).await;
    upload_all(updater, pair_with_store(cache_entries, store), None).await
}

/// Every entry paired with the local store it is packed from: the shape
/// [`upload_all`] takes, for the callers that push paths they already hold.
fn pair_with_store<'a>(
    entries: Vec<CachedPath>,
    store: &'a LocalNixStore,
) -> Vec<(CachedPath, nar::NarSource<'a>)> {
    entries
        .into_iter()
        .map(|cp| (cp, nar::NarSource::Path { store: Some(store) }))
        .collect()
}

/// The `inputSrcs` declared by each `.drv`, read by parsing the file directly
/// rather than via the daemon's reference walk (which is unreliable for a
/// `.drv`'s sources). Mirrors the build-side prefetch so every source a build
/// worker will demand is pushed by the eval that produced it. A `.drv` that
/// cannot be read or parsed is skipped (logged), not fatal - the daemon closure
/// still covers it.
async fn drv_input_sources(drv_paths: &[String]) -> std::collections::HashSet<String> {
    use futures::stream::{self, StreamExt as _};
    use gradient_sources::nix_store_path;

    const DRV_READ_CONCURRENCY: usize = 64;

    stream::iter(drv_paths.iter().cloned())
        .map(|drv_path| async move {
            let full = nix_store_path(&drv_path);
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

/// Paths one job uploads at once. The per-path resume round trip dominates a
/// push of many small `.drv` and source NARs, and the server stages by
/// `(peer, job, hash)`, so overlapping streams cannot collide.
pub(crate) const UPLOAD_CONCURRENCY: usize = 4;

/// Upload one path's NAR using the method the server advertised in its
/// `CacheQuery {Push}` response: a presigned S3 PUT straight to object storage
/// when available, else the chunked WS `NarPush` fallback (local stores).
/// Already-cached paths are skipped. Errors are returned so the caller decides
/// whether they are fatal.
///
/// The `source` says where the bytes come from: a path of the local store, or a
/// raw NAR already in memory.
pub(crate) async fn upload_one_nar(
    updater: &JobUpdater,
    cp: &CachedPath,
    source: nar::NarSource<'_>,
) -> Result<()> {
    match cp.as_info() {
        CachedPathInfo::Cached { .. } => {
            tracing::debug!(store_path = %cp.path, "skipping NAR upload - already cached");
            Ok(())
        }
        CachedPathInfo::Uncached { path, upload } => {
            nar::upload_nar(
                &updater.job_id,
                path,
                source,
                nar::NarSink::from_upload_target(upload, &updater.nar_recv),
                &updater.writer,
            )
            .await
        }
    }
}

/// Upload every uncached entry, at most [`UPLOAD_CONCURRENCY`] at once, failing
/// the whole set on the first error. `abort` is re-checked before each path so a
/// server-side `AbortJob` stops the remaining uploads.
pub(crate) async fn upload_all(
    updater: &JobUpdater,
    uploads: Vec<(CachedPath, nar::NarSource<'_>)>,
    abort: Option<&watch::Receiver<bool>>,
) -> Result<()> {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let pending = uploads.iter().filter(|(cp, _)| !cp.cached).count();
    if pending == 0 {
        return Ok(());
    }

    // One span for the batch: the uploads overlap, and the timeline parents a
    // span to the innermost open one, so per-path spans would chart as nested
    // and count their durations twice.
    let mut guard = updater.phase(JobPhase::NarPush);
    guard.record(pending as u32, 0);

    let mut queued = uploads.into_iter();
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < UPLOAD_CONCURRENCY {
            let Some((cp, source)) = queued.next() else {
                break;
            };
            running.push(upload_unless_aborted(updater, cp, source, abort));
        }

        match running.next().await {
            Some(result) => result?,
            None => return Ok(()),
        }
    }
}

async fn upload_unless_aborted(
    updater: &JobUpdater,
    cp: CachedPath,
    source: nar::NarSource<'_>,
    abort: Option<&watch::Receiver<bool>>,
) -> Result<()> {
    if let Some(abort) = abort {
        check_abort(abort)?;
    }

    let result = upload_one_nar(updater, &cp, source).await;
    if result.is_err()
        && let Some(abort) = abort
    {
        // An abort cancels the push-resume gate mid-upload, so re-check before
        // blaming the path: the failure is the abort, and it must stay typed.
        check_abort(abort)?;
    }
    result
}

/// The `(name, path)` of every output a spec actually names.
fn named_outputs(task: &BuildSpec) -> Vec<(String, String)> {
    task.outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect()
}

/// Split what the local store already holds out of what a job
/// would go and get or build. An output on disk is already realised, so producing it again
/// costs a round trip to answer a question the store answers for free - and on a
/// host with no route out, the fetch is not an answer at all. A worker with no
/// daemon errors on every ask and fetches everything, exactly as before.
async fn split_already_realised<S: WorkerStore + ?Sized>(
    store: &S,
    wanted: Vec<(String, String)>,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut realised = Vec::new();
    let mut fetch = Vec::new();
    for (name, path) in wanted {
        if store.has_path(&path).await.unwrap_or(false) {
            realised.push((name, path));
        } else {
            fetch.push((name, path));
        }
    }

    (realised, fetch)
}

fn fully_realised(realised: &[(String, String)], missing: &[(String, String)]) -> bool {
    missing.is_empty() && !realised.is_empty()
}

/// The report for outputs nobody had to fetch. Sizes stay `None` for the compress
/// step to fill in, the way a real build reports what it just wrote.
async fn realised_outputs(realised: &[(String, String)]) -> Vec<BuildOutput> {
    let mut outputs = Vec::with_capacity(realised.len());
    for (name, store_path) in realised {
        outputs.push(realised_output(name, store_path).await);
    }
    outputs
}

async fn realised_output(name: &str, store_path: &str) -> BuildOutput {
    BuildOutput {
        name: name.to_owned(),
        store_path: store_path.to_owned(),
        hash: gradient_sources::get_hash_from_path(store_path.to_owned())
            .map(|(h, _)| h)
            .unwrap_or_default(),
        nar_size: None,
        nar_hash: None,
        products: build::load_products(store_path).await,
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
    pub(crate) log_limits: crate::executor::log_limit::LogRateLimits,
    pub(crate) log_fetch_from_store: bool,
    pub(crate) build_cores: u32,
}

impl JobExecutor {
    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new(
        store: LocalNixStore,
        evaluator: WorkerEvaluator,
        gcroots: GcRootKeeper,
        binpath_nix: String,
        binpath_ssh: String,
        build_metrics: bool,
        build_cgroup_root: String,
        log_limits: crate::executor::log_limit::LogRateLimits,
        log_fetch_from_store: bool,
        build_cores: u32,
    ) -> Self {
        Self {
            store: Arc::new(store),
            evaluator: Arc::new(evaluator),
            gcroots,
            binpath_nix,
            binpath_ssh,
            build_metrics,
            build_cgroup_root,
            log_limits,
            log_fetch_from_store,
            build_cores,
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
    /// When `FetchFlake` and eval steps are in the same job, the local clone
    /// path from the fetch is reused for evaluation - the repo is cloned
    /// exactly once.
    #[instrument(skip_all, fields(steps = ?job.steps))]
    pub async fn execute_flake_job(
        &self,
        job: FlakeJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: watch::Receiver<bool>,
    ) -> Result<()> {
        // If FetchFlake runs, it stores the local checkout path here so
        // subsequent eval steps use it instead of the remote URL.
        let mut local_flake_path: Option<String> = None;

        for step in &job.steps {
            match step {
                FlakeStep::FetchFlake => {
                    let _fetch = updater.phase(JobPhase::Fetch);
                    // A Cached build source lives only in the gradient cache;
                    // substitute it locally so `nix flake archive path:<store_path>`
                    // can read it before archiving its inputs with credentials.
                    if let Some(src) = eval::required_local_source(&job.source) {
                        crate::proto::prefetch::ensure_path(&self.store, src, updater).await?;
                    }

                    let outcome = fetch::fetch_repository(
                        &job,
                        updater as &mut dyn gradient_proto::traits::JobReporter,
                        credentials,
                        &self.binpath_nix,
                        &self.binpath_ssh,
                        abort.clone(),
                    )
                    .await?;

                    let sizes = vec![None; outcome.archived_paths.len()];
                    let cache_entries =
                        query_fetched_paths(updater, outcome.archived_paths.clone(), sizes).await;
                    {
                        let mut push = updater.phase(JobPhase::PushInputs);
                        push.record(cache_entries.len() as u32, 0);
                        upload_all(updater, pair_with_store(cache_entries, &self.store), None)
                            .await?;
                    }

                    updater
                        .report_fetch_result(outcome.flake_source.clone())
                        .await?;
                    local_flake_path = Some(outcome.local_flake_path);
                }
                FlakeStep::EvaluateFlake => {
                    let _g = updater.phase(JobPhase::EvalFlake);
                    eval::evaluate_flake(&job, updater).await?
                }
                FlakeStep::EvaluateDerivations => {
                    // A `Cached` source was archived to a *different* worker's
                    // store and pushed to the cache; substitute it locally
                    // before eval, since nix won't pull a `path:` flake ref
                    // from a binary cache.
                    if local_flake_path.is_none()
                        && let Some(src) = eval::required_local_source(&job.source)
                    {
                        crate::proto::prefetch::ensure_path(&self.store, src, updater).await?;
                    }

                    // Each batch's `.drv` runtime closure (input_sources + .drvs,
                    // for narinfo substitution and downstream-build prefetch) is
                    // pushed to the cache inside the walk, before that batch's
                    // `report_eval_result` - so #392's mid-eval build dispatch
                    // never races the source upload. The server keys cached_path
                    // by hash, so NAR/row ordering is irrelevant.
                    let _g = updater.phase(JobPhase::EvalDerivations);
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
    async fn adopt_realised<'a>(
        &'a self,
        build_task: &BuildSpec,
        task_index: u32,
        realised: Vec<(String, String)>,
        updater: &mut JobUpdater,
        gc_handles: &mut Vec<GcRootHandle>,
        outputs: &mut Vec<compress::OutputNar<'a>>,
    ) -> Result<()> {
        if self.log_fetch_from_store {
            build::forward_store_build_log(updater, task_index, &build_task.drv_path).await;
        }
        for (_, path) in &realised {
            gc_handles.push(self.gcroots.add(path).await);
        }
        let reported = realised_outputs(&realised).await;
        updater
            .report_build_output(build_task.build_id.clone(), reported, None, true)
            .await?;
        outputs.extend(
            realised
                .into_iter()
                .map(|(_, store_path)| compress::OutputNar {
                    store_path,
                    source: nar::NarSource::Path {
                        store: Some(&self.store),
                    },
                }),
        );
        Ok(())
    }

    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater,
        _credentials: &CredentialStore,
        mut abort: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut outputs: Vec<compress::OutputNar<'_>> = Vec::new();
        let mut gc_handles: Vec<GcRootHandle> = Vec::new();
        for (index, build_task) in job.builds.iter().enumerate() {
            check_abort(&abort)?;
            // Move the build to `Building` on the server *before* anything
            // that can fail. The state machine only allows
            // `Building → Failed`; if we let prefetch (or anything before
            // `report_building`) bubble up an error first, the eventual
            // `JobFailed` would arrive at the server while the build is
            // still `Queued`, the transition would be rejected, and the UI
            // would show the build hanging in `Queued` forever.
            updater.report_building(build_task.build_id.clone()).await?;

            let (realised, missing) =
                split_already_realised(self.store.as_ref(), named_outputs(build_task)).await;
            if fully_realised(&realised, &missing) {
                self.adopt_realised(
                    build_task,
                    index as u32,
                    realised,
                    updater,
                    &mut gc_handles,
                    &mut outputs,
                )
                .await?;
                continue;
            }

            if build_task.kind == BuildSpecKind::Substitute {
                // What the store already held is pinned before the fetch that runs
                // beside it; what an upstream serves never lands there, so it needs
                // no root.
                for (_, path) in &realised {
                    gc_handles.push(self.gcroots.add(path).await);
                }

                let mut progress = updater.download_progress(build_task.build_id.clone());
                let fetched = substitute::fetch_outputs(
                    &mut substitute::JobUpdaterIo(updater),
                    &missing,
                    &mut progress,
                )
                .await
                .map_err(|e| failure::classify_substitute_failure(&build_task.build_id, e))?;

                let mut reported = realised_outputs(&realised).await;
                reported.extend(fetched.iter().map(|f| {
                    BuildOutput {
                        name: f.name.clone(),
                        store_path: f.store_path.clone(),
                        hash: gradient_sources::get_hash_from_path(f.store_path.clone())
                            .map(|(h, _)| h)
                            .unwrap_or_default(),
                        nar_size: f.nar.as_ref().map(|n| n.nar.len() as i64),
                        nar_hash: f.nar.as_ref().map(|n| nar::sha256_nix32(&n.nar)),
                        products: Vec::new(),
                    }
                }));
                updater
                    .report_build_output(build_task.build_id.clone(), reported, None, true)
                    .await?;

                outputs.extend(
                    realised
                        .into_iter()
                        .map(|(_, store_path)| compress::OutputNar {
                            store_path,
                            source: nar::NarSource::Path {
                                store: Some(&self.store),
                            },
                        }),
                );
                outputs.extend(fetched.into_iter().filter_map(|f| {
                    f.nar.map(|raw| compress::OutputNar {
                        store_path: f.store_path,
                        source: nar::NarSource::Raw {
                            nar: raw.nar,
                            references: raw.references,
                            deriver: raw.deriver,
                            ca: raw.ca,
                        },
                    })
                }));
                continue;
            }

            if build_task.kind == BuildSpecKind::Download {
                let (store_path, raw) = {
                    let _phase = updater.phase(JobPhase::Download);
                    let mut progress = updater.download_progress(build_task.build_id.clone());
                    download::download_output(
                        &mut download::JobUpdaterIo(updater),
                        build_task,
                        &mut progress,
                    )
                    .await
                    .map_err(|e| failure::classify_download_failure(&build_task.build_id, e))?
                };
                let reported = vec![BuildOutput {
                    name: "out".to_owned(),
                    store_path: store_path.clone(),
                    hash: gradient_sources::get_hash_from_path(store_path.clone())
                        .map(|(h, _)| h)
                        .unwrap_or_default(),
                    nar_size: Some(raw.nar.len() as i64),
                    nar_hash: Some(nar::sha256_nix32(&raw.nar)),
                    products: Vec::new(),
                }];
                updater
                    .report_build_output(build_task.build_id.clone(), reported, None, false)
                    .await?;
                outputs.push(compress::OutputNar {
                    store_path,
                    source: nar::NarSource::Raw {
                        nar: raw.nar,
                        references: raw.references,
                        deriver: raw.deriver,
                        ca: raw.ca,
                    },
                });
                continue;
            }

            // Pin the .drv as an indirect GC root before prefetching its
            // inputs. Nix's reachability walks .drv references
            // (input_drvs + input_sources), so one root covers the entire
            // build-time closure. A Substitute never fetches the .drv, so this
            // only applies to real builds.
            gc_handles.push(self.gcroots.add(&build_task.drv_path).await);

            // Import cache-resident inputs the daemon will need. A hard
            // local-store error (e.g. `store.has_path` failing) aborts the
            // build - we can't safely proceed without knowing what's already
            // in the store. Other prefetch errors (CacheQuery transport,
            // individual NAR downloads) are logged inside `prefetch_inputs`
            // and don't reach here as `Err`.
            {
                let _g = updater.phase(JobPhase::Prefetch);
                crate::proto::prefetch::prefetch_inputs(&self.store, build_task, updater)
                    .await
                    .map_err(|e| failure::classify_prefetch_error(&build_task.build_id, e))?;
            }

            let _build = updater.phase(JobPhase::Build);
            let built = build::build_derivation(
                &self.store,
                build_task,
                index as u32,
                updater,
                &mut abort,
                self.build_metrics,
                &self.build_cgroup_root,
                self.log_limits,
                self.log_fetch_from_store,
                self.build_cores,
            )
            .await?;
            for o in &built {
                gc_handles.push(self.gcroots.add(&o.store_path).await);
            }
            outputs.extend(built.into_iter().map(|o| compress::OutputNar {
                store_path: o.store_path,
                source: nar::NarSource::Path {
                    store: Some(&self.store),
                },
            }));
        }

        // Always compress+push every realised output. The worker is the sole
        // producer of compressed NARs; the server stores them and computes
        // narinfo signatures from the uploaded metadata. Honours `abort`
        // between paths so an `AbortJob` from the server (e.g. session NAR
        // buffer exceeded) terminates the upload loop and surfaces as a
        // `JobFailed`.
        {
            let mut compress = updater.phase(JobPhase::Compress);
            compress.record(outputs.len() as u32, 0);
            compress::push_outputs(updater, outputs, &abort)
                .await
                .map_err(failure::BuildError::transient)?;
        }

        // Release every indirect GC root for this job; symlinks are removed
        // and the daemon's next GC walk is free to delete unreachable paths.
        drop(gc_handles);
        Ok(())
    }
}

/// Future that resolves only when the abort signal becomes `true`.
///
/// Uses `changed()` + `borrow()` (not `wait_for`) to avoid holding a
/// non-`Send` `Ref<'_, bool>` guard across an await point.
///
/// If the sender is dropped (e.g. in tests using a receiver without a sender),
/// the future parks forever instead of treating the drop as an abort.
pub(crate) async fn abort_true(abort: &mut watch::Receiver<bool>) {
    loop {
        match abort.changed().await {
            Ok(()) => {
                if *abort.borrow() {
                    return;
                }
            }
            // Sender dropped - treat as "no abort", park forever.
            Err(_) => std::future::pending::<()>().await,
        }
    }
}

/// Propagate a server-side `AbortJob` as an error so the surrounding job
/// resolves to `JobFailed` instead of `JobCompleted`. Typed so the failure
/// classifier reports `BuildFailureKind::Aborted` rather than treating it as an
/// unclassified `Permanent` failure.
pub(crate) fn check_abort(abort: &watch::Receiver<bool>) -> Result<()> {
    if *abort.borrow() {
        return Err(failure::JobAborted("job aborted by server".to_owned()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::worker_store::FakeWorkerStore;

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

    fn spec(kind: BuildSpecKind, outputs: &[(&str, &str)]) -> BuildSpec {
        BuildSpec {
            build_id: "b".to_owned(),
            drv_path: "/nix/store/x.drv".to_owned(),
            kind,
            is_fixed_output: false,
            outputs: outputs
                .iter()
                .map(|(name, path)| gradient_proto::messages::DerivationOutput {
                    name: (*name).to_owned(),
                    path: (*path).to_owned(),
                })
                .collect(),
            timeout_secs: None,
            max_silent_secs: None,
        }
    }

    /// An output with no path is not a path to look for: the spec names it, but
    /// there is nothing to ask the store about and nothing to pack.
    #[test]
    fn named_outputs_drops_the_ones_with_no_path() {
        let task = spec(
            BuildSpecKind::Substitute,
            &[("out", "/nix/store/a-out"), ("dev", "")],
        );

        assert_eq!(
            named_outputs(&task),
            vec![("out".to_owned(), "/nix/store/a-out".to_owned())]
        );
    }

    /// The reason the check exists: an output already on disk must not be fetched.
    /// A Download that reaches for its URL anyway fails on a host with no route
    /// out, which is every hermetic test VM and every offline builder.
    #[tokio::test]
    async fn what_the_store_already_holds_is_not_fetched() {
        let store = FakeWorkerStore::new().with_present_path("/nix/store/a-out");
        let wanted = vec![
            ("out".to_owned(), "/nix/store/a-out".to_owned()),
            ("dev".to_owned(), "/nix/store/b-dev".to_owned()),
        ];

        let (realised, missing) = split_already_realised(&store, wanted).await;

        assert_eq!(
            realised,
            vec![("out".to_owned(), "/nix/store/a-out".to_owned())]
        );
        assert_eq!(
            missing,
            vec![("dev".to_owned(), "/nix/store/b-dev".to_owned())]
        );
    }

    struct NoDaemon;

    #[async_trait::async_trait]
    impl WorkerStore for NoDaemon {
        async fn has_path(&self, _store_path: &str) -> Result<bool> {
            Err(anyhow::anyhow!("acquire daemon connection: no such file"))
        }
    }

    /// A Download runs on workers that have no nix at all, which is the point of
    /// the kind. The store that cannot answer must not swallow the output: every
    /// path falls through to the fetch it would have had before this check.
    #[tokio::test]
    async fn a_worker_without_a_daemon_fetches_everything() {
        let wanted = vec![("out".to_owned(), "/nix/store/a-out".to_owned())];

        let (realised, missing) = split_already_realised(&NoDaemon, wanted.clone()).await;

        assert!(realised.is_empty());
        assert_eq!(missing, wanted);
    }

    /// A Build whose outputs are all on disk has nothing to build; handing it to the
    /// daemon anyway costs a round trip, and a partial or pathless one still builds.
    #[test]
    fn only_a_build_with_every_output_on_disk_is_skipped() {
        let out = || ("out".to_owned(), "/nix/store/a-out".to_owned());
        let dev = || ("dev".to_owned(), "/nix/store/b-dev".to_owned());

        assert!(fully_realised(&[out(), dev()], &[]));
        assert!(!fully_realised(&[out()], &[dev()]));
        assert!(!fully_realised(&[], &[]));
    }

    /// The sizes are left for the compress step, the way a real build reports the
    /// outputs it just wrote; the hash is the store path's own.
    #[tokio::test]
    async fn a_realised_output_reports_no_sizes() {
        let out = realised_output("out", "/nix/store/xa1b2c3-thing").await;

        assert_eq!(out.name, "out");
        assert_eq!(out.store_path, "/nix/store/xa1b2c3-thing");
        assert!(out.nar_size.is_none() && out.nar_hash.is_none());
    }

    /// An output adopted from disk keeps the hydra products a fresh build would report.
    #[tokio::test]
    async fn a_realised_output_reports_its_hydra_products() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().to_str().unwrap();
        let tarball = dir.path().join("foo.tar.gz");
        std::fs::write(&tarball, b"tar").unwrap();
        std::fs::create_dir(dir.path().join("nix-support")).unwrap();
        std::fs::write(
            dir.path().join("nix-support/hydra-build-products"),
            format!("file binary-dist {}\n", tarball.display()),
        )
        .unwrap();

        let out = realised_output("out", store_path).await;

        assert_eq!(out.products.len(), 1);
        assert_eq!(out.products[0].name, "foo.tar.gz");
        assert_eq!(out.products[0].size, Some(3));
    }
}
