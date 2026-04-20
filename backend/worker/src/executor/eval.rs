/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation tasks — Nix flake attribute discovery and derivation closure walk.
//!
//! The worker uses an in-process `EvalWorkerPool` (subprocess pool running the
//! Nix C API isolated from Tokio) to do the actual evaluation.  The results are
//! transmitted back to the server as [`DiscoveredDerivation`] structs.
//!
//! No database access occurs here — all DB writes are done server-side when the
//! server receives the `EvalResult` [`JobUpdateKind`].

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::worker_pool::WorkerPoolResolver;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt as _};
use gradient_core::db::parse_drv;
use gradient_core::nix::DerivationResolver;
use proto::messages::{DerivationOutput, DiscoveredDerivation, FlakeJob, FlakeSource};
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Abort error returned from the eval pipeline when the dispatch loop fires
/// the watch signal in response to a server-side `AbortJob`. Bubbles up as a
/// regular `Err`, which the worker translates into `JobFailed` — the server's
/// `handle_eval_job_failed` then no-ops because the eval is already
/// `Aborted` from the API call.
const ABORT_ERR: &str = "evaluation aborted by server";

/// Returns true if the dispatch loop has flipped the abort watch to `true`.
fn is_aborted(abort: &mut watch::Receiver<bool>) -> bool {
    *abort.borrow_and_update()
}

/// How many `.drv` files to read+parse concurrently inside a single BFS wave.
/// Reading a `.drv` is async filesystem IO, so the sequential walk would only
/// keep one in-flight read at a time and bottleneck on round-trip latency.
/// Pulling a wave of paths and resolving them in parallel cuts wall-clock
/// closure-walk time by roughly the concurrency factor for IO-bound stores
/// (network FS, slow disks). Cap kept low to avoid open-fd / kernel pressure.
const DRV_READ_CONCURRENCY: usize = 64;

use proto::messages::QueryMode;

use crate::proto::job::JobUpdater;
use crate::traits::{DrvReader, FsDrvReader, JobReporter};

/// Drives Nix evaluation inside the worker.
///
/// Uses a pool of eval subprocess workers (one `NixEvaluator` per subprocess)
/// to isolate the Nix C API from the async runtime.
pub struct WorkerEvaluator {
    resolver: Arc<WorkerPoolResolver>,
}

impl WorkerEvaluator {
    /// Create a new evaluator with a pool of `eval_workers` subprocesses.
    pub fn new(eval_workers: usize, max_evals_per_worker: usize) -> Self {
        Self {
            resolver: Arc::new(WorkerPoolResolver::new(eval_workers, max_evals_per_worker)),
        }
    }
}

impl Clone for WorkerEvaluator {
    fn clone(&self) -> Self {
        Self {
            resolver: self.resolver.clone(),
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
    updater.report_evaluating_flake()
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
) -> Result<Vec<String>> {
    evaluate_derivations_with(
        &*evaluator.resolver,
        &FsDrvReader,
        job,
        local_flake_path,
        updater,
        abort,
    )
    .await
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
/// starts with `/nix/store/` — content-addressed and immutable, valid in pure
/// eval mode.  For a temporary `/tmp/` checkout we use `git+file://?rev=` to
/// stay pure (bare `path:/tmp/...` would allow impure `builtins.fetchGit`
/// calls that bypass `builtins.tryEval`).
fn build_flake_url(job: &FlakeJob, local_flake_path: Option<&str>) -> String {
    if let Some(path) = local_flake_path {
        if path.starts_with("/nix/store/") {
            return format!("path:{}", path);
        }
        // A tmp git checkout — pair it with the commit from source when we
        // know we're on a Repository source; else fall back to a bare
        // `path:` reference.
        if let FlakeSource::Repository { commit, .. } = &job.source {
            return format!("git+file://{}?rev={}", path, commit);
        }
        return format!("path:{}", path);
    }
    match &job.source {
        FlakeSource::Repository { url, commit } => {
            gradient_core::nix::NixFlakeUrl::new(url, commit)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| url.clone())
        }
        // Eval-only: Nix accepts `/nix/store/...` directly as a flake URI.
        FlakeSource::Cached { store_path } => format!("path:{}", store_path),
    }
}

/// Read and parse every `.drv` in `wave` concurrently, preserving BFS order.
///
/// A read or parse failure is a hard error — silently dropping a derivation
/// drops its entire dep subtree, causing the dispatcher to release the parent
/// prematurely and the nix-daemon to die with "1 dependency failed".
async fn parse_drv_wave(
    drv_reader: &dyn DrvReader,
    wave: &[(Option<String>, String)],
) -> Result<Vec<gradient_core::db::Derivation>> {
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

    let mut slots: Vec<Option<gradient_core::db::Derivation>> =
        (0..wave.len()).map(|_| None).collect();
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
    drv: &gradient_core::db::Derivation,
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

    DiscoveredDerivation {
        attr: attr.unwrap_or_default(),
        drv_path,
        outputs,
        dependencies,
        architecture: drv.system.clone(),
        required_features: drv.required_system_features(),
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
    /// Every `.drv` path the walker actually parsed (i.e. present in the
    /// local store, not pruned via `known_set`). The caller uses this to
    /// push each drv NAR into the cache and sign it.
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
                // Server already has the full subtree — report the derivation
                // (so a build row is created) but skip further traversal.
                self.batch.push(DiscoveredDerivation {
                    attr: String::new(),
                    drv_path: dep,
                    outputs: vec![],
                    dependencies: vec![],
                    architecture: String::new(),
                    required_features: vec![],
                    substituted: true, // already built — skip dispatch
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

            // Mid-walk flush: let the server start queuing builds early.
            if self.batch.len() >= EVAL_BATCH_SIZE {
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
) -> Result<Vec<String>> {
    if is_aborted(abort) {
        anyhow::bail!(ABORT_ERR);
    }
    updater.report_evaluating_derivations().await?;

    let repo = build_flake_url(job, local_flake_path);

    // ── Step 1: discover attr paths ──────────────────────────────────────────
    debug!(repo = %repo, "listing flake derivations");
    let (attrs, mut warnings) = match resolver
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
        updater.report_eval_result(vec![], warnings, vec![]).await?;
        return Ok(Vec::new());
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
    let mut errors: Vec<String> = Vec::new();
    for (attr, result) in resolved {
        match result {
            Ok((drv_path, _refs)) => root_drvs.push((attr, drv_path)),
            Err(e) => {
                warn!(attr, error = %e, "failed to resolve attr; skipping");
                errors.push(format!("failed to resolve {attr}: {e}"));
            }
        }
    }

    if root_drvs.is_empty() {
        warn!("all attr resolutions failed");
        updater.report_eval_result(vec![], warnings, errors).await?;
        return Ok(Vec::new());
    }

    // ── Step 3+4+5: BFS closure walk with incremental flushes ────────────────
    let mut walker = ClosureWalker::new(drv_reader, &root_drvs);
    let mut remaining = walker.walk(updater, abort).await?;
    let produced_drvs = std::mem::take(&mut walker.produced_drvs);

    // ── Final flush: remaining derivations + deduplicated warnings/errors ─────
    warnings.sort_unstable();
    warnings.dedup();
    errors.sort_unstable();
    errors.dedup();

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
    Ok(produced_drvs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use test_support::fakes::derivation_resolver::FakeDerivationResolver;
    use test_support::prelude::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test-store")
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
        }
    }

    /// Set up resolver and drv_reader from a StoreFixture.
    /// Tests don't fire abort: hand back a receiver from a sender we drop on
    /// the floor. `is_aborted` reads `*borrow_and_update()` which stays
    /// `false` (the initial value) — the sender being dropped doesn't flip
    /// it, so abort never triggers in tests.
    fn never_abort() -> watch::Receiver<bool> {
        let (_tx, rx) = watch::channel(false);
        rx
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
    /// "aborting jobs does not work when in EvaluatingDerivation state" —
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
