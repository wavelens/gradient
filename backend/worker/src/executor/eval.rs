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
use proto::messages::{DerivationOutput, DiscoveredDerivation, FlakeJob};
use tracing::{debug, info, warn};

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
        Self { resolver: self.resolver.clone() }
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
) -> Result<()> {
    evaluate_derivations_with(
        &*evaluator.resolver,
        &FsDrvReader,
        job,
        local_flake_path,
        updater,
    )
    .await
}

/// Query the server cache for `batch`'s output paths and set `substituted`
/// on any derivation whose outputs are all present in the cache.
async fn mark_substituted(batch: &mut Vec<DiscoveredDerivation>, updater: &mut dyn JobReporter) {
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
            && drv.outputs.iter().all(|o| cached_set.contains(o.path.as_str()))
        {
            drv.substituted = true;
        }
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
) -> Result<()> {
    updater.report_evaluating_derivations().await?;

    // Use the local clone from FetchFlake if available, otherwise build a
    // commit-pinned remote URL.
    //
    // When FetchFlake archived the repo into the Nix store (via `nix flake
    // archive`), the returned path starts with `/nix/store/`.  Nix store paths
    // are content-addressed and immutable, so `path:/nix/store/xxx` is valid in
    // pure evaluation mode — we use it directly.
    //
    // When FetchFlake fell back to a temporary git checkout in /tmp, use
    // `git+file://?rev=` to keep Nix in pure mode: `path:/tmp/...` would switch
    // Nix to impure mode, allowing `builtins.fetchGit` calls without a `rev` to
    // attempt live network fetches that are not catchable by `builtins.tryEval`.
    let repo = if let Some(path) = local_flake_path {
        if path.starts_with("/nix/store/") {
            format!("path:{}", path)
        } else {
            format!("git+file://{}?rev={}", path, job.commit)
        }
    } else {
        gradient_core::nix::NixFlakeUrl::new(&job.repository, &job.commit)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| job.repository.clone())
    };
    let wildcards = job.wildcards.clone();

    // ── Step 1: discover attr paths ──────────────────────────────────────────
    debug!(repo = %repo, "listing flake derivations");
    let (attrs, mut warnings) = resolver
        .list_flake_derivations(repo.clone(), wildcards)
        .await
        .context("list_flake_derivations failed")?;

    if attrs.is_empty() {
        warn!("no derivations found for evaluation");
        updater.report_eval_result(vec![], warnings, vec![]).await?;
        return Ok(());
    }

    // ── Step 2: resolve attr paths → drv paths ───────────────────────────────
    let (resolved, resolve_warnings) = resolver
        .resolve_derivation_paths(repo.clone(), attrs.clone())
        .await
        .context("resolve_derivation_paths failed")?;
    warnings.extend(resolve_warnings);

    // Build (attr, drv_path) pairs from successful resolutions.
    // Per-attr failures are hard errors (not Nix warnings) — they prevent the
    // derivation from being built at all, so they go into `errors`.
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
        return Ok(());
    }

    // ── Step 3+4+5: BFS closure walk with incremental EvalResult flushing ───────
    // Start from all root drv paths.  Every EVAL_BATCH_SIZE derivations the
    // worker queries the server cache and sends an EvalResult so the server can
    // start queuing builds while the walk continues.
    let mut batch: Vec<DiscoveredDerivation> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(Option<String>, String)> = VecDeque::new(); // (attr, drv_path)

    for (attr, drv) in &root_drvs {
        if visited.insert(drv.clone()) {
            queue.push_back((Some(attr.clone()), drv.clone()));
        }
    }

    info!(
        roots = root_drvs.len(),
        "starting closure walk"
    );
    let walk_start = Instant::now();
    let mut walked: usize = 0;

    // Drive the BFS in waves: drain up to DRV_READ_CONCURRENCY items from the
    // queue, read+parse them concurrently, then fold the results back into
    // `batch` / `visited` / `queue` sequentially (so those structures need no
    // locking). Reading a single `.drv` is async filesystem IO and almost all
    // of the per-derivation cost — running waves concurrently scales the walk
    // from "one inflight read at a time" to many.
    while !queue.is_empty() {
        // Drain a wave from the queue, preserving BFS order within the wave.
        let wave_size = queue.len().min(DRV_READ_CONCURRENCY);
        let mut wave: Vec<(Option<String>, String)> = Vec::with_capacity(wave_size);
        for _ in 0..wave_size {
            wave.push(queue.pop_front().expect("wave_size <= queue.len()"));
        }

        // Read + parse every .drv in the wave concurrently. Indices keep the
        // results aligned with `wave` so later sequential processing is
        // deterministic and BFS order is preserved across waves.
        // CRITICAL: A read or parse failure must abort the eval — silently
        // dropping a derivation drops its entire dep subtree from the closure
        // walk, so the server never sees those dependencies, the dispatch
        // SQL releases the parent prematurely, and the nix-daemon then dies
        // with "1 dependency failed".
        let mut futs = wave
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
            .collect::<FuturesUnordered<_>>();

        let mut parsed: Vec<Option<gradient_core::db::Derivation>> =
            (0..wave.len()).map(|_| None).collect();
        while let Some(result) = futs.next().await {
            let (i, drv) = result?;
            parsed[i] = Some(drv);
        }
        drop(futs); // release any borrows of drv_reader before re-using it

        for ((attr, drv_path), drv_opt) in wave.into_iter().zip(parsed) {
            let drv = drv_opt.expect("every wave slot was filled");

            // Enqueue input derivations (BFS frontier expansion).
            for (input_drv, _outputs) in &drv.input_derivations {
                if visited.insert(input_drv.clone()) {
                    queue.push_back((None, input_drv.clone()));
                }
            }

            let outputs: Vec<DerivationOutput> = drv
                .outputs
                .iter()
                .filter(|o| !o.path.is_empty())
                .map(|o| DerivationOutput { name: o.name.clone(), path: o.path.clone() })
                .collect();

            let dependencies: Vec<String> = drv
                .input_derivations
                .iter()
                .map(|(p, _)| p.clone())
                .collect();

            batch.push(DiscoveredDerivation {
                attr: attr.unwrap_or_default(),
                drv_path: drv_path.clone(),
                outputs,
                dependencies,
                architecture: drv.system.clone(),
                required_features: drv.required_system_features(),
                substituted: false,
            });

            walked += 1;
            // Heartbeat log every 500 derivations so the operator can tell
            // "slow eval" apart from "stuck eval".
            if walked.is_multiple_of(500) {
                info!(
                    walked,
                    queued = queue.len(),
                    elapsed_secs = walk_start.elapsed().as_secs(),
                    "closure walk progress"
                );
            }

            // Flush a mid-walk batch once it reaches EVAL_BATCH_SIZE.
            if batch.len() >= EVAL_BATCH_SIZE {
                mark_substituted(&mut batch, updater).await;
                debug!(count = batch.len(), remaining = queue.len(), "flushing eval batch");
                updater.report_eval_result(std::mem::take(&mut batch), vec![], vec![]).await?;
            }
        }
    }

    info!(
        walked,
        elapsed_secs = walk_start.elapsed().as_secs(),
        "closure walk complete"
    );

    // ── Final flush: remaining derivations + accumulated warnings/errors ──────
    warnings.sort_unstable();
    warnings.dedup();
    errors.sort_unstable();
    errors.dedup();

    mark_substituted(&mut batch, updater).await;
    debug!(
        count = batch.len(),
        warnings = warnings.len(),
        errors = errors.len(),
        "closure walk complete — flushing final batch"
    );
    updater.report_eval_result(batch, warnings, errors).await
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
            repository: repo.into(),
            commit: "abc123".into(),
            wildcards: vec!["*".into()],
            timeout_secs: None,
            sign: None,
        }
    }

    /// Set up resolver and drv_reader from a StoreFixture.
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

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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
        let entry = all.iter().find(|d| d.drv_path == fixture.entry_point).unwrap();
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

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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

        let err = evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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

    #[tokio::test]
    async fn test_eval_dependencies_match_fixture() {
        let fixture = load_store(&fixture_dir());
        let repo = "https://example.com/repo";
        let (resolver, drv_reader) = setup_from_fixture(&fixture, repo, "hello");
        let job = make_flake_job(repo);
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
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
