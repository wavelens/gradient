/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation tasks - Nix flake attribute discovery and derivation closure walk.
//!
//! The worker uses an in-process `EvalWorkerPool` (subprocess pool running the
//! Nix C API isolated from Tokio) to do the actual evaluation.  The results are
//! transmitted back to the server as [`DiscoveredDerivation`] structs.
//!
//! No database access occurs here - all DB writes are done server-side when the
//! server receives the `EvalResult` [`JobUpdateKind`].

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::worker_pool::{WorkerPoolResolver, budgeted_pool_size};
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt as _};
use gradient_db::parse_drv;
use gradient_nix::{DerivationResolver, FlakeDiscovery};
use gradient_proto::messages::{
    DerivationOutput, DiscoveredDerivation, EvalAttrCost, EvalStatsReport, FlakeJob,
    FlakeOutputNode, FlakeSource,
};
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Abort error returned from the eval pipeline when the dispatch loop fires
/// the watch signal in response to a server-side `AbortJob`. Bubbles up as a
/// regular `Err`, which the worker translates into `JobFailed` - the server's
/// `handle_eval_job_failed` then no-ops because the eval is already
/// `Aborted` from the API call.
const ABORT_ERR: &str = "evaluation aborted by server";

/// Returns true if the dispatch loop has flipped the abort watch to `true`.
fn is_aborted(abort: &mut watch::Receiver<bool>) -> bool {
    *abort.borrow_and_update()
}

/// The set of attr paths the user requested explicitly: patterns with no
/// `*`/`#` wildcard segment (and not an exclusion). A drvPath-resolution
/// failure on one of these is a genuine error; failures on wildcard-expanded
/// attrs are skipped, since a wildcard spans attrs that aren't derivations.
fn explicit_attr_set(wildcards: &[String]) -> HashSet<String> {
    wildcards
        .iter()
        .filter_map(|w| {
            let (exclude, segs) = crate::nix::wildcard_walk::parse_pattern(w);
            (!exclude && !segs.iter().any(|s| s == "*" || s == "#")).then(|| segs.join("."))
        })
        .collect()
}

/// Errors for explicitly requested attrs that discovery matched to nothing.
/// Empty when every pattern was a wildcard (a wildcard legitimately spans attrs
/// that aren't buildable), so only a pinpointed target fails the eval instead of
/// silently completing with no outputs.
fn unmatched_target_errors(wildcards: &[String]) -> Vec<String> {
    let mut errors: Vec<String> = explicit_attr_set(wildcards)
        .into_iter()
        .map(|attr| format!("target '{attr}' matched no derivations in the flake"))
        .collect();
    errors.sort_unstable();
    errors
}

/// How many `.drv` files to read+parse concurrently inside a single BFS wave.
/// Reading a `.drv` is async filesystem IO, so the sequential walk would only
/// keep one in-flight read at a time and bottleneck on round-trip latency.
/// Pulling a wave of paths and resolving them in parallel cuts wall-clock
/// closure-walk time by roughly the concurrency factor for IO-bound stores
/// (network FS, slow disks). Cap kept low to avoid open-fd / kernel pressure.
const DRV_READ_CONCURRENCY: usize = 64;

/// Fraction of host RAM the eval pool may occupy (`pool_size * max_eval_rss`),
/// leaving headroom for the OS, the parent worker, and a concurrent build.
const EVAL_RAM_SHARE: f64 = 0.75;

/// Assumed host RAM when `sysinfo` cannot read it (containers without
/// /proc/meminfo): small enough to keep the pool conservative.
const TOTAL_RAM_FALLBACK_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Total physical RAM in bytes, or [`TOTAL_RAM_FALLBACK_BYTES`] if unreadable.
fn total_memory_bytes() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let total = sys.total_memory();
    if total == 0 {
        TOTAL_RAM_FALLBACK_BYTES
    } else {
        total
    }
}

use gradient_proto::messages::QueryMode;

use crate::proto::job::JobUpdater;
use crate::traits::{DrvReader, FsDrvReader, JobReporter};

/// Drives Nix evaluation inside the worker.
///
/// Uses a pool of eval subprocess workers (one `NixEvaluator` per subprocess)
/// to isolate the Nix C API from the async runtime.
pub struct WorkerEvaluator {
    resolver: Arc<WorkerPoolResolver>,
    /// When set, the worker pulls/pushes the flake's shared eval-cache blob
    /// around evaluation (issue #386 L3).
    eval_cache_share: bool,
}

impl WorkerEvaluator {
    /// Create a new evaluator. `fork_workers` (env `GRADIENT_EVAL_FORK_WORKERS`)
    /// is the pool size and thus the eval concurrency; `eval_workers` is legacy.
    /// `eval_cache_dir` is exported to every eval worker as `NIX_CACHE_HOME`.
    pub fn new(
        _eval_workers: usize,
        fork_workers: usize,
        max_eval_rss: u64,
        min_free_ram_mb: u64,
        eval_cache_dir: String,
        eval_cache_share: bool,
    ) -> Self {
        // Size the pool so `pool_size * max_eval_rss` stays within a fraction of
        // host RAM: a flake with many systems then evaluates in parallel without
        // OOM, falling back to fewer shards (down to one) on a small host. Shards
        // share one eval-cache safely because per-shard commits only append to
        // the WAL (no checkpoint); the single end-of-eval checkpoint folds it in.
        let total_ram = total_memory_bytes();
        let ram_budget = (total_ram as f64 * EVAL_RAM_SHARE) as u64;
        let pool_size = budgeted_pool_size(fork_workers, max_eval_rss, ram_budget);
        if pool_size < fork_workers {
            info!(
                fork_workers,
                pool_size,
                max_eval_rss,
                ram_budget,
                "eval pool sized down to fit the memory budget"
            );
        }

        // `max_eval_rss` only recycles between calls; the reaper is the peak
        // guard that kills a runaway eval before the host OOMs (issue: OOM
        // kills not registered by the server).
        let min_free_bytes = crate::worker_pool::memory_guard_bytes(min_free_ram_mb, total_ram);
        let resolver = Arc::new(WorkerPoolResolver::new(
            pool_size,
            max_eval_rss,
            eval_cache_dir,
        ));
        resolver.start_memory_reaper(min_free_bytes);

        Self {
            resolver,
            eval_cache_share,
        }
    }

    /// Gracefully shut every idle eval-worker subprocess down.
    pub async fn shutdown(&self) {
        self.resolver.shutdown().await;
    }
}

impl Clone for WorkerEvaluator {
    fn clone(&self) -> Self {
        Self {
            resolver: self.resolver.clone(),
            eval_cache_share: self.eval_cache_share,
        }
    }
}

/// Advance status to `EvaluatingFlake`.
///
/// Attr discovery is done inside [`evaluate_derivations`] since the server
/// only cares about the final [`DiscoveredDerivation`] list.
pub async fn evaluate_flake(_job: &FlakeJob, updater: &mut JobUpdater) -> Result<()> {
    // Rebind as &JobUpdater so the inherent &self method wins over the &mut self
    // trait method that async_trait generates.
    let updater: &JobUpdater = updater;
    updater.report_evaluating_flake().await
}

/// For a `Cached` source, the store path that must be present locally before
/// evaluation (substituted from the gradient cache if absent). A `Repository`
/// source is fetched/cloned by the worker itself, so nothing to ensure.
pub fn required_local_source(source: &FlakeSource) -> Option<&str> {
    match source {
        FlakeSource::Cached { store_path } => Some(store_path.as_str()),
        FlakeSource::Repository { .. } => None,
    }
}

/// Number of derivations to accumulate before flushing a mid-walk `EvalResult`.
const EVAL_BATCH_SIZE: usize = 50;

/// Walk the full derivation closure and report [`DiscoveredDerivation`]s to
/// the server incrementally.
///
/// This is the main evaluation step:
/// 1. Discover attr paths (via eval worker pool)
/// 2. Resolve attrs to .drv paths (via eval worker pool)
/// 3. BFS from root .drv paths through `inputDrvs` references
/// 4. For each .drv: read file, extract outputs/arch/features
/// 5. Every `EVAL_BATCH_SIZE` derivations: query server cache, mark
///    substituted, send `EvalResult` so the server can start queuing builds
///    while the walk continues
/// 6. Final flush with any remainder + accumulated warnings/errors
pub async fn evaluate_derivations(
    evaluator: &WorkerEvaluator,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut JobUpdater,
    abort: &mut watch::Receiver<bool>,
) -> Result<()> {
    let repo = build_flake_url(job, local_flake_path);
    let start = Instant::now();

    // Best-effort fingerprint + pull of the flake's shared eval-cache blob so
    // the eval runs warm. A fingerprint/pull failure never fails the eval.
    let fingerprint = if evaluator.eval_cache_share {
        match evaluator.resolver.fingerprint(repo.clone()).await {
            Ok(fp) => fp,
            Err(e) => {
                warn!(error = %e, "eval-cache fingerprint failed; evaluating local-only");
                None
            }
        }
    } else {
        None
    };

    let cache_path = fingerprint.as_ref().map(|fp| {
        format!(
            "{}/eval-cache-v6/{fp}.sqlite",
            evaluator.resolver.eval_cache_dir()
        )
    });

    if let (Some(fp), Some(path)) = (fingerprint.as_ref(), cache_path.as_ref()) {
        // TODO(#386): report cache_status (hit/miss) once an eval-update field exists
        match updater.pull_eval_cache(fp).await {
            Ok(Some(bytes)) => {
                if let Err(e) = write_eval_cache_blob(path, &bytes).await {
                    warn!(error = %e, %path, "failed to stage pulled eval-cache blob");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "eval-cache pull failed; evaluating local-only"),
        }
    }

    let EvalOutcome { flake_nodes } = evaluate_derivations_with(
        &*evaluator.resolver,
        &FsDrvReader,
        job,
        local_flake_path,
        updater,
        abort,
    )
    .await?;

    // Drain the per-eval stats and ship one report; skip if nothing was
    // observed (metrics gated off or an empty eval).
    let totals = evaluator.resolver.take_eval_stats();
    if totals.total_thunks > 0 || !totals.per_entry_point.is_empty() {
        let report =
            build_eval_stats_report(totals, flake_nodes, start.elapsed().as_millis() as u64);
        if let Err(e) = updater.report_eval_stats(report).await {
            warn!(error = %e, "failed to send eval stats report");
        }
    }

    // Fold the shared eval-cache WAL into the main `.sqlite` so the pushed blob
    // carries this eval's writes (per-shard commits only append to the WAL). The
    // checkpoint is PASSIVE: it never blocks, so it is safe even when another
    // evaluation of the same flake is concurrently reading the cache. Best-effort.
    if cache_path.is_some()
        && let Err(e) = evaluator.resolver.checkpoint_cache(repo.clone()).await
    {
        warn!(error = %e, "eval-cache checkpoint failed; pushing as-is");
    }

    if let (Some(fp), Some(path)) = (fingerprint.as_ref(), cache_path.as_ref())
        && let Ok(bytes) = tokio::fs::read(path).await
        && let Err(e) = updater.push_eval_cache(fp, bytes).await
    {
        warn!(error = %e, "eval-cache push failed; continuing");
    }

    Ok(())
}

/// Write a pulled eval-cache blob to `path`, creating its parent directory.
async fn write_eval_cache_blob(path: &str, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(path, bytes).await?;
    Ok(())
}

/// Query the server cache for `batch`'s output paths and set `substituted`
/// on any derivation whose outputs are all present in the cache.
async fn mark_substituted(batch: &mut [DiscoveredDerivation], updater: &mut dyn JobReporter) {
    let output_paths: Vec<String> = batch
        .iter()
        .flat_map(|d| d.outputs.iter().map(|o| o.path.clone()))
        .collect();
    if output_paths.is_empty() {
        return;
    }
    let cached = updater
        .query_cache(output_paths, QueryMode::Normal)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "cache query failed; treating all paths as uncached");
            vec![]
        });
    let cached_set: HashSet<&str> = cached.iter().map(|c| c.path.as_str()).collect();
    for drv in batch.iter_mut() {
        if !drv.outputs.is_empty()
            && drv
                .outputs
                .iter()
                .all(|o| cached_set.contains(o.path.as_str()))
        {
            drv.substituted = true;
        }
    }
}

// ── Pipeline helpers ─────────────────────────────────────────────────────────

/// Build the flake reference string from a job and an optional local checkout.
///
/// When `FetchFlake` archived the repo into the Nix store the returned path
/// starts with `/nix/store/` - content-addressed and immutable, valid in pure
/// eval mode.  For a temporary `/tmp/` checkout we use `git+file://?rev=` to
/// stay pure (bare `path:/tmp/...` would allow impure `builtins.fetchGit`
/// calls that bypass `builtins.tryEval`).
fn build_flake_url(job: &FlakeJob, local_flake_path: Option<&str>) -> String {
    if let Some(path) = local_flake_path {
        if path.starts_with("/nix/store/") {
            return format!("path:{}", path);
        }
        // A tmp git checkout - pair it with the commit from source when we
        // know we're on a Repository source; else fall back to a bare
        // `path:` reference.
        if let FlakeSource::Repository { commit, .. } = &job.source {
            return format!("git+file://{}?rev={}", path, commit);
        }
        return format!("path:{}", path);
    }
    match &job.source {
        FlakeSource::Repository { url, commit } => gradient_nix::NixFlakeUrl::new(url, commit)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| url.clone()),
        // Eval-only: Nix accepts `/nix/store/...` directly as a flake URI.
        FlakeSource::Cached { store_path } => format!("path:{}", store_path),
    }
}

/// Read and parse every `.drv` in `wave` concurrently, preserving BFS order.
///
/// A read or parse failure is a hard error - silently dropping a derivation
/// drops its entire dep subtree, causing the dispatcher to release the parent
/// prematurely and the nix-daemon to die with "1 dependency failed".
async fn parse_drv_wave(
    drv_reader: &dyn DrvReader,
    wave: &[(Option<String>, String)],
) -> Result<Vec<gradient_db::Derivation>> {
    // Index-tagged futures so results can be sorted back into BFS order.
    let mut futs: FuturesUnordered<_> = wave
        .iter()
        .enumerate()
        .map(|(i, (_, drv_path))| {
            let drv_path = drv_path.clone();
            async move {
                let bytes = drv_reader.read_drv(&drv_path).await.with_context(|| {
                    format!(
                        "cannot read .drv {drv_path} during closure walk; aborting eval \
                         to avoid silently dropping dependencies"
                    )
                })?;
                let parsed = parse_drv(&bytes).with_context(|| {
                    format!(
                        "cannot parse .drv {drv_path} during closure walk; aborting eval \
                         to avoid silently dropping dependencies"
                    )
                })?;
                Ok::<_, anyhow::Error>((i, parsed))
            }
        })
        .collect();

    let mut slots: Vec<Option<gradient_db::Derivation>> = (0..wave.len()).map(|_| None).collect();
    while let Some(result) = futs.next().await {
        let (i, drv) = result?;
        slots[i] = Some(drv);
    }

    Ok(slots
        .into_iter()
        .map(|s| s.expect("every wave slot was filled"))
        .collect())
}

/// Build a [`DiscoveredDerivation`] from a parsed `.drv` file.
fn build_discovered_derivation(
    attr: Option<String>,
    drv_path: String,
    drv: &gradient_db::Derivation,
) -> DiscoveredDerivation {
    let outputs: Vec<DerivationOutput> = drv
        .outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| DerivationOutput {
            name: o.name.clone(),
            path: o.path.clone(),
        })
        .collect();

    let dependencies: Vec<String> = drv
        .input_derivations
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

    let input_sources = drv.input_sources.clone();

    let meta = drv.build_meta();
    let name = drv
        .environment
        .get("name")
        .map(String::as_str)
        .unwrap_or("");
    let pname = gradient_db::derive_pname(drv.environment.get("pname").map(String::as_str), name);
    DiscoveredDerivation {
        attr: attr.unwrap_or_default(),
        drv_path,
        outputs,
        dependencies,
        input_sources,
        architecture: drv.system.clone(),
        required_features: meta.required_features,
        timeout_secs: meta.timeout_secs,
        max_silent_secs: meta.max_silent_secs,
        prefer_local_build: meta.prefer_local_build,
        is_fixed_output: meta.is_fixed_output,
        allow_substitutes: drv.allow_substitutes(),
        pname,
        substituted: false,
    }
}

/// BFS closure walker.
///
/// Holds the walk state (frontier queue, visited set, accumulation batch)
/// and drives the traversal in concurrent waves of up to
/// [`DRV_READ_CONCURRENCY`] `.drv` paths.
///
/// Every [`EVAL_BATCH_SIZE`] derivations the batch is flushed to the server
/// so builds can start queuing while the walk continues.
struct ClosureWalker<'a> {
    drv_reader: &'a dyn DrvReader,
    batch: Vec<DiscoveredDerivation>,
    visited: HashSet<String>,
    queue: VecDeque<(Option<String>, String)>,
    walked: usize,
    start: Instant,
    /// `.drv` paths the walker parsed since the last flush (present in the
    /// local store, not pruned via `known_set`). Drained at each flush to push
    /// the batch's runtime closure to the cache *before* its `report_eval_result`
    /// so a mid-eval build dispatch never races the source upload.
    produced_drvs: Vec<String>,
}

impl<'a> ClosureWalker<'a> {
    /// Initialise the walker with `root_drvs` as the BFS frontier.
    fn new(drv_reader: &'a dyn DrvReader, root_drvs: &[(String, String)]) -> Self {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        for (attr, drv) in root_drvs {
            if visited.insert(drv.clone()) {
                queue.push_back((Some(attr.clone()), drv.clone()));
            }
        }
        info!(roots = root_drvs.len(), "starting closure walk");
        Self {
            drv_reader,
            batch: Vec::new(),
            visited,
            queue,
            walked: 0,
            start: Instant::now(),
            produced_drvs: Vec::new(),
        }
    }

    /// Drive the full BFS, flushing intermediate batches to `updater`.
    ///
    /// Returns the final unflushed batch; the caller is responsible for the
    /// final `report_eval_result` call.
    async fn walk(
        &mut self,
        updater: &mut dyn JobReporter,
        abort: &mut watch::Receiver<bool>,
    ) -> Result<Vec<DiscoveredDerivation>> {
        while !self.queue.is_empty() {
            // Honour AbortJob at every wave boundary.
            if is_aborted(abort) {
                anyhow::bail!(ABORT_ERR);
            }
            self.process_wave(updater).await?;
        }

        info!(
            walked = self.walked,
            elapsed_secs = self.start.elapsed().as_secs(),
            "closure walk complete"
        );

        Ok(std::mem::take(&mut self.batch))
    }

    /// Drain one concurrent wave from the front of the queue and process it.
    async fn process_wave(&mut self, updater: &mut dyn JobReporter) -> Result<()> {
        let wave_size = self.queue.len().min(DRV_READ_CONCURRENCY);
        let wave: Vec<_> = (0..wave_size)
            .map(|_| self.queue.pop_front().expect("wave_size <= queue.len()"))
            .collect();

        let parsed_drvs = parse_drv_wave(self.drv_reader, &wave).await?;

        // Collect all new input-derivation paths from this wave so we can
        // batch-query the server once rather than once per derivation.
        let mut new_deps: Vec<String> = Vec::new();
        for drv in &parsed_drvs {
            for (input_drv, _) in &drv.input_derivations {
                if !self.visited.contains(input_drv.as_str()) {
                    new_deps.push(input_drv.clone());
                }
            }
        }
        new_deps.sort_unstable();
        new_deps.dedup();

        // Pre-mark ALL new deps as visited so nothing adds them twice.
        for dep in &new_deps {
            self.visited.insert(dep.clone());
        }

        // Ask the server which deps it already has.  Known deps don't need
        // subtree traversal; we add them as minimal DiscoveredDerivation
        // entries so the server can still create build rows for them.
        let known_set: HashSet<String> = if new_deps.is_empty() {
            HashSet::new()
        } else {
            updater
                .query_known_derivations(new_deps.clone())
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "query_known_derivations failed; treating all as unknown");
                    vec![]
                })
                .into_iter()
                .collect()
        };

        if !known_set.is_empty() {
            debug!(pruned = known_set.len(), "BFS: pruning known subtrees");
        }

        // Enqueue unknown deps; add known deps to the batch directly.
        for dep in new_deps {
            if known_set.contains(&dep) {
                // Server already has the full subtree - report the derivation
                // (so a build row is created) but skip further traversal.
                self.batch.push(DiscoveredDerivation {
                    attr: String::new(),
                    drv_path: dep,
                    outputs: vec![],
                    dependencies: vec![],
                    input_sources: vec![],
                    architecture: String::new(),
                    required_features: vec![],
                    timeout_secs: None,
                    max_silent_secs: None,
                    prefer_local_build: false,
                    is_fixed_output: false,
                    allow_substitutes: true,
                    pname: None,
                    substituted: true, // already built - skip dispatch
                });
            } else {
                self.queue.push_back((None, dep));
            }
        }

        for ((attr, drv_path), drv) in wave.into_iter().zip(parsed_drvs) {
            self.produced_drvs.push(drv_path.clone());
            self.batch
                .push(build_discovered_derivation(attr, drv_path, &drv));
            self.walked += 1;

            // Heartbeat log so operators can distinguish "slow eval" from "stuck".
            if self.walked.is_multiple_of(500) {
                info!(
                    walked = self.walked,
                    queued = self.queue.len(),
                    elapsed_secs = self.start.elapsed().as_secs(),
                    "closure walk progress"
                );
            }

            // Mid-walk flush: let the server start queuing builds early. Push
            // this batch's `.drv` runtime closure (input_sources + .drvs)
            // BEFORE reporting it, so once #392 promotes and dispatches these
            // builds mid-eval their sources are already in the cache.
            if self.batch.len() >= EVAL_BATCH_SIZE {
                updater.push_drv_closure(&self.produced_drvs).await?;
                self.produced_drvs.clear();
                mark_substituted(&mut self.batch, updater).await;
                debug!(
                    count = self.batch.len(),
                    remaining = self.queue.len(),
                    "flushing eval batch"
                );
                updater
                    .report_eval_result(std::mem::take(&mut self.batch), vec![], vec![])
                    .await?;
            }
        }

        Ok(())
    }
}

// ── Orchestrator ──────────────────────────────────────────────────────────────

/// Result of one closure-walk eval: the walked flake-output graph for eval
/// metrics. Each batch's `.drv` runtime closure is pushed to the cache during
/// the walk (before its `report_eval_result`), not by the caller afterwards.
#[derive(Debug)]
pub struct EvalOutcome {
    pub flake_nodes: Vec<FlakeOutputNode>,
}

/// Classify a flake-output attr path into a coarse node kind from its top-level
/// segment, matching the metric tables' `kind` column.
fn flake_kind(attr: &str) -> &'static str {
    match attr.split('.').next().unwrap_or("") {
        "packages" | "legacyPackages" => "package",
        "devShells" | "devShell" => "devShell",
        "checks" => "check",
        "apps" => "app",
        "nixosConfigurations" => "nixosConfiguration",
        _ => "other",
    }
}

/// Build flake-output nodes from the resolved entry-point attrs. Each resolved
/// root is a derivation leaf; its `parent` is the dotted path minus the last
/// segment. No extra evaluation: only attrs the discovery walk already produced.
fn flake_nodes_from_roots(root_drvs: &[(String, String)]) -> Vec<FlakeOutputNode> {
    root_drvs
        .iter()
        .map(|(attr, drv)| {
            let (parent, name) = match attr.rsplit_once('.') {
                Some((p, n)) => (Some(p.to_string()), n.to_string()),
                None => (None, attr.clone()),
            };

            FlakeOutputNode {
                path: attr.clone(),
                parent,
                name,
                kind: flake_kind(attr).to_string(),
                is_derivation: true,
                drv_path: Some(drv.clone()),
            }
        })
        .collect()
}

/// Convert the resolver's accumulated totals into the wire report. Per-entry
/// `eval_ms` is not tracked by the aggregator, so it stays 0; phase timings and
/// `worker_id` are filled by the caller.
fn build_eval_stats_report(
    totals: crate::worker_pool::eval_stats::EvalStatsTotals,
    flake_nodes: Vec<FlakeOutputNode>,
    total_eval_ms: u64,
) -> EvalStatsReport {
    const MB: u64 = 1024 * 1024;
    EvalStatsReport {
        total_thunks: totals.total_thunks,
        fn_calls: totals.fn_calls,
        primop_calls: totals.primop_calls,
        lookups: totals.lookups,
        alloc_bytes: totals.alloc_bytes,
        peak_heap_mb: totals.peak_heap_bytes / MB,
        peak_rss_mb: totals.peak_rss_bytes / MB,
        total_eval_ms,
        per_entry_point: totals
            .per_entry_point
            .into_iter()
            .map(|c| EvalAttrCost {
                attr: c.attr,
                thunks: c.thunks,
                fn_calls: c.fn_calls,
                eval_ms: 0,
                alloc_bytes: c.alloc_bytes,
            })
            .collect(),
        flake_nodes,
        ..Default::default()
    }
}

/// Testable version of [`evaluate_derivations`] that accepts trait objects.
///
/// All concrete dependencies are replaced with trait objects so this function
/// can be exercised in unit tests with fakes (no real nix-daemon, no real
/// filesystem, no real WebSocket connection).
///
/// When `local_flake_path` is `Some`, the evaluator uses `path:<local_path>`
/// as the flake reference instead of building a remote URL from
/// `job.repository` + `job.commit`. This is the case when `FetchFlake` already
/// cloned the repo in the same `FlakeJob`.
pub async fn evaluate_derivations_with(
    resolver: &dyn DerivationResolver,
    drv_reader: &dyn DrvReader,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut dyn JobReporter,
    abort: &mut watch::Receiver<bool>,
) -> Result<EvalOutcome> {
    if is_aborted(abort) {
        anyhow::bail!(ABORT_ERR);
    }
    updater.report_evaluating_derivations().await?;

    let repo = build_flake_url(job, local_flake_path);

    // ── Step 1: discover attr paths ──────────────────────────────────────────
    debug!(repo = %repo, "listing flake derivations");
    let FlakeDiscovery {
        attrs,
        mut warnings,
        mut errors,
    } = match resolver
        .list_flake_derivations(repo.clone(), job.wildcards.clone())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            // Surface the Nix error as an EvalResult so it appears in the UI,
            // not just as an opaque JobFailed summary.
            let err_msg = format!("list_flake_derivations failed: {:#}", e);
            warn!(error = %err_msg, "reporting eval error to server");
            let _ = updater
                .report_eval_result(vec![], vec![], vec![err_msg])
                .await;
            return Err(e).context("list_flake_derivations failed");
        }
    };

    if attrs.is_empty() {
        warn!("no derivations found for evaluation");
        errors.extend(unmatched_target_errors(&job.wildcards));
        errors.sort_unstable();
        errors.dedup();
        updater.report_eval_result(vec![], warnings, errors).await?;
        return Ok(EvalOutcome {
            flake_nodes: Vec::new(),
        });
    }

    if is_aborted(abort) {
        anyhow::bail!(ABORT_ERR);
    }

    // ── Step 2: resolve attr paths → drv paths ───────────────────────────────
    let (resolved, resolve_warnings) =
        match resolver.resolve_derivation_paths(repo.clone(), attrs).await {
            Ok(v) => v,
            Err(e) => {
                // Forward warnings accumulated so far so they aren't lost.
                let err_msg = format!("resolve_derivation_paths failed: {:#}", e);
                warn!(error = %err_msg, "reporting eval error to server");
                let _ = updater
                    .report_eval_result(vec![], warnings, vec![err_msg])
                    .await;
                return Err(e).context("resolve_derivation_paths failed");
            }
        };
    warnings.extend(resolve_warnings);

    let mut root_drvs: Vec<(String, String)> = Vec::new();
    for (attr, result) in resolved {
        match result {
            Ok((drv_path, _refs)) => root_drvs.push((attr, drv_path)),
            Err(e) => errors.push(format!("failed to resolve {attr}: {e}")),
        }
    }

    if root_drvs.is_empty() {
        warn!("all attr resolutions failed");
        errors.sort_unstable();
        errors.dedup();
        updater.report_eval_result(vec![], warnings, errors).await?;
        return Ok(EvalOutcome {
            flake_nodes: Vec::new(),
        });
    }

    let flake_nodes = flake_nodes_from_roots(&root_drvs);

    // ── Step 3+4+5: BFS closure walk with incremental flushes ────────────────
    let mut walker = ClosureWalker::new(drv_reader, &root_drvs);
    let mut remaining = walker.walk(updater, abort).await?;
    let remaining_drvs = std::mem::take(&mut walker.produced_drvs);

    // ── Final flush: remaining derivations + deduplicated warnings/errors ─────
    warnings.sort_unstable();
    warnings.dedup();
    errors.sort_unstable();
    errors.dedup();

    // Push the trailing batch's closure before its report, same as the mid-walk
    // flushes, so the last builds' sources are cached before dispatch.
    updater.push_drv_closure(&remaining_drvs).await?;
    mark_substituted(&mut remaining, updater).await;
    debug!(
        count = remaining.len(),
        warnings = warnings.len(),
        errors = errors.len(),
        "flushing final eval batch"
    );
    updater
        .report_eval_result(remaining, warnings, errors)
        .await?;
    Ok(EvalOutcome { flake_nodes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::derivation_resolver::FakeDerivationResolver;
    use gradient_test_support::prelude::*;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test-store")
    }

    #[test]
    fn unmatched_explicit_target_errors_but_wildcard_is_silent() {
        assert_eq!(
            unmatched_target_errors(&["packages.x86_64-linux.uxc".to_string()]),
            vec![
                "target 'packages.x86_64-linux.uxc' matched no derivations in the flake"
                    .to_string()
            ]
        );
        assert!(unmatched_target_errors(&["packages.x86_64-linux.#".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["packages.x86_64-linux.*".to_string()]).is_empty());
        assert!(unmatched_target_errors(&["!nixosConfigurations.foo".to_string()]).is_empty());
    }

    fn make_flake_job(repo: &str) -> FlakeJob {
        FlakeJob {
            tasks: vec![],
            source: FlakeSource::Repository {
                url: repo.into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        }
    }

    /// Set up resolver and drv_reader from a StoreFixture.
    /// Tests don't fire abort: hand back a receiver from a sender we drop on
    /// the floor. `is_aborted` reads `*borrow_and_update()` which stays
    /// `false` (the initial value) - the sender being dropped doesn't flip
    /// it, so abort never triggers in tests.
    fn never_abort() -> watch::Receiver<bool> {
        let (_tx, rx) = watch::channel(false);
        rx
    }

    #[test]
    fn explicit_attr_set_keeps_only_wildcard_free_includes() {
        let set = explicit_attr_set(&[
            "packages.x86_64-linux.hello".into(),
            "packages.x86_64-linux.#".into(),
            "packages.x86_64-linux.*".into(),
            "!packages.x86_64-linux.broken".into(),
            "checks.\"py.3\".unit".into(),
        ]);

        assert!(set.contains("packages.x86_64-linux.hello"));
        assert!(
            set.contains("checks.py.3.unit"),
            "quoted dots collapse to the discovered path form"
        );
        assert!(
            !set.contains("packages.x86_64-linux.broken"),
            "exclusions are not explicit requests"
        );
        assert_eq!(set.len(), 2, "wildcard patterns contribute nothing");
    }

    fn setup_from_fixture(
        fixture: &StoreFixture,
        repo: &str,
        attr: &str,
    ) -> (FakeDerivationResolver, FakeDrvReader) {
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs(repo, vec![attr.to_string()])
            .with_drv_path(repo, attr, fixture.entry_point.clone());

        let drv_reader = FakeDrvReader::from_raw_drvs(fixture.raw_drvs.clone());

        (resolver, drv_reader)
    }

    #[test]
    fn cached_source_requires_store_path_present() {
        let cached = FlakeSource::Cached {
            store_path: "/nix/store/abc-source".into(),
        };
        assert_eq!(
            required_local_source(&cached),
            Some("/nix/store/abc-source")
        );
        let repo = FlakeSource::Repository {
            url: "u".into(),
            commit: "c".into(),
        };
        assert_eq!(required_local_source(&repo), None);
    }

    #[test]
    fn build_discovered_derivation_carries_input_sources() {
        let drv = gradient_db::Derivation {
            outputs: vec![gradient_db::DerivationOutput {
                name: "out".into(),
                path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
                hash_algo: String::new(),
                hash: String::new(),
            }],
            input_derivations: vec![],
            input_sources: vec![
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-pipewire-extra-config".into(),
                "/nix/store/cccccccccccccccccccccccccccccccc-source-stdenv.sh".into(),
            ],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: std::collections::HashMap::new(),
        };

        let discovered = build_discovered_derivation(
            Some("attr".into()),
            "/nix/store/dddddddddddddddddddddddddddddddd-foo.drv".into(),
            &drv,
        );

        assert_eq!(discovered.input_sources, drv.input_sources);
    }

    #[tokio::test]
    async fn test_eval_closure_walk_empty_store() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        // Should have at least EvaluatingDerivations + one EvalResult.
        assert!(reporter.len() >= 2);

        let all = reporter.all_eval_derivations();
        // All derivations from the fixture should be discovered across all batches.
        assert_eq!(all.len(), fixture.derivations.len());
        // Nothing is built → nothing substituted.
        assert!(all.iter().all(|d| !d.substituted));
        // Entry point should have the attr set.
        let entry = all
            .iter()
            .find(|d| d.drv_path == fixture.entry_point)
            .unwrap();
        assert_eq!(entry.attr, "hello");
        // Warnings should be empty for a valid fixture (check final batch).
        if let ReportedEvent::EvalResult { warnings, .. } = reporter.last_eval_result().unwrap() {
            assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);
        }
    }

    /// Regression (#392): every parsed derivation's `.drv` runtime closure must
    /// be pushed to the cache BEFORE the batch that reports it. Reporting a
    /// derivation is what lets the server promote+dispatch its build mid-eval,
    /// so a build worker would otherwise prefetch input_sources that aren't in
    /// the cache yet ("required input path missing").
    #[tokio::test]
    async fn pushes_batch_closure_before_reporting_it() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let mut pushed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for event in &reporter.events {
            match event {
                ReportedEvent::DrvClosurePush { drv_paths } => {
                    pushed.extend(drv_paths.iter().map(|s| s.as_str()));
                }
                ReportedEvent::EvalResult { derivations, .. } => {
                    for d in derivations {
                        // Known/substituted entries are already cached server-side
                        // and carry no closure to push; only parsed drvs matter.
                        assert!(
                            d.substituted || pushed.contains(d.drv_path.as_str()),
                            "reported {} before pushing its source closure",
                            d.drv_path
                        );
                    }
                }
                _ => {}
            }
        }

        assert!(!pushed.is_empty(), "expected at least one closure push");
    }

    #[tokio::test]
    async fn test_eval_partial_substitution() {
        let mut fixture = load_store(&fixture_dir());
        // Build ~50% of derivations.
        fixture.mark_all_built();
        fixture.remove_random_subtrees(0.5, 42);

        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        // Simulate the same subset being cached on the server.
        let cached: Vec<String> = fixture.store.present_paths().into_iter().collect();
        let mut reporter = RecordingJobReporter::new().with_cached_paths(cached);

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let all = reporter.all_eval_derivations();
        let substituted_count = all.iter().filter(|d| d.substituted).count();
        let not_substituted = all.iter().filter(|d| !d.substituted).count();
        assert!(substituted_count > 0, "some should be substituted");
        assert!(not_substituted > 0, "some should not be substituted");

        // Verify substituted flags match the fixture's built state.
        for drv in &all {
            let is_built = fixture.built().iter().any(|b| b.drv_path == drv.drv_path);
            assert_eq!(
                drv.substituted, is_built,
                "substituted mismatch for {}: got {} expected {}",
                drv.drv_path, drv.substituted, is_built
            );
        }
    }

    #[tokio::test]
    async fn test_eval_all_substituted() {
        let mut fixture = load_store(&fixture_dir());
        fixture.mark_all_built();

        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        // Simulate all output paths being present in the server's cache.
        let cached: Vec<String> = fixture.store.present_paths().into_iter().collect();
        let mut reporter = RecordingJobReporter::new().with_cached_paths(cached);

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let all = reporter.all_eval_derivations();
        assert!(
            all.iter().all(|d| d.substituted),
            "all should be substituted when everything is cached"
        );
    }

    #[tokio::test]
    async fn test_eval_empty_attrs() {
        let resolver = FakeDerivationResolver::new();
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job("https://example.com/empty");
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        if let ReportedEvent::EvalResult { derivations, .. } = reporter.last_eval_result().unwrap()
        {
            assert!(derivations.is_empty());
        } else {
            panic!("expected EvalResult");
        }
    }

    #[tokio::test]
    async fn test_eval_missing_drv_fails_loudly() {
        // Resolver resolves an attr to a drv path that doesn't exist in the reader.
        // Silently skipping would drop dependency edges and let the dispatcher
        // release the parent build prematurely, so we MUST surface this as a
        // hard eval failure instead of a warning.
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs("repo", vec!["pkg".into()])
            .with_drv_path("repo", "pkg", "/nix/store/nonexistent.drv");
        let drv_reader = FakeDrvReader::new(); // No drvs loaded
        let job = make_flake_job("repo");
        let mut reporter = RecordingJobReporter::new();

        let err = evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .expect_err("missing .drv must abort the eval");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonexistent.drv"),
            "error should mention the missing path: {msg}"
        );
        assert!(
            msg.contains("aborting eval"),
            "error should explain the abort: {msg}"
        );
    }

    /// When the abort watch is flipped before eval starts, the function
    /// returns immediately without doing any work. Regression guard for
    /// "aborting jobs does not work when in EvaluatingDerivation state" -
    /// previously `evaluate_derivations_with` ignored the abort signal
    /// entirely.
    #[tokio::test]
    async fn test_eval_aborts_when_signal_set_before_start() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        let (tx, rx) = watch::channel(false);
        let mut abort = rx;
        tx.send(true).unwrap();

        let err = evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut abort,
        )
        .await
        .expect_err("aborted eval must return Err");
        assert!(
            format!("{err:#}").contains("aborted by server"),
            "error should mention abort: {err:#}"
        );
        // We should have bailed before sending an EvaluatingDerivations
        // status update, so the reporter records nothing.
        assert!(reporter.is_empty(), "reporter should not see any events");
    }

    #[tokio::test]
    async fn wildcard_resolve_failure_is_reported() {
        let repo = "https://example.com/repo";
        // Attr is discovered but has no drv path - fake resolve returns Err.
        // The job is a pure wildcard, so this is NOT an explicit target.
        let resolver = FakeDerivationResolver::new().with_flake_attrs(repo, vec!["broken".into()]);
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job(repo); // wildcards: ["*"]
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let ReportedEvent::EvalResult { errors, .. } = reporter.last_eval_result().unwrap() else {
            panic!("expected an EvalResult");
        };
        assert!(
            errors.iter().any(|e| e.contains("broken")),
            "wildcard resolve failure must be surfaced: {errors:?}"
        );
    }

    #[tokio::test]
    async fn discovery_errors_reach_eval_result() {
        let repo = "https://example.com/repo";
        // No attrs discovered, but discovery recorded a thrown-attr diagnostic.
        let resolver = FakeDerivationResolver::new()
            .with_flake_errors(repo, vec!["failed to evaluate 'x': boom".into()]);
        let drv_reader = FakeDrvReader::new();
        let job = make_flake_job(repo); // wildcard job, no unmatched-target noise
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let ReportedEvent::EvalResult { errors, .. } = reporter.last_eval_result().unwrap() else {
            panic!("expected an EvalResult");
        };
        assert!(
            errors.iter().any(|e| e.contains("boom")),
            "discovery errors must reach the server: {errors:?}"
        );
    }

    #[tokio::test]
    async fn test_eval_dependencies_match_fixture() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(
            &resolver,
            &drv_reader,
            &job,
            None,
            &mut reporter,
            &mut never_abort(),
        )
        .await
        .unwrap();

        let all = reporter.all_eval_derivations();
        {
            // Build a dependency map from all eval result batches.
            let eval_deps: std::collections::HashMap<&str, Vec<&str>> = all
                .iter()
                .map(|d| {
                    (
                        d.drv_path.as_str(),
                        d.dependencies.iter().map(|s| s.as_str()).collect(),
                    )
                })
                .collect();

            // Compare against fixture tree.
            for drv in &fixture.derivations {
                let eval_dep_list = eval_deps
                    .get(drv.drv_path.as_str())
                    .unwrap_or_else(|| panic!("missing {} in eval result", drv.drv_path));
                let fixture_dep_list = fixture.tree.get(&drv.drv_path).unwrap();

                let mut eval_sorted: Vec<&str> = eval_dep_list.clone();
                eval_sorted.sort();
                let mut fixture_sorted: Vec<&str> =
                    fixture_dep_list.iter().map(|s| s.as_str()).collect();
                fixture_sorted.sort();

                assert_eq!(
                    eval_sorted, fixture_sorted,
                    "dependency mismatch for {}",
                    drv.drv_path
                );
            }
        }
    }
}
