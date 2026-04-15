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

use crate::worker_pool::WorkerPoolResolver;
use anyhow::{Context, Result};
use gradient_core::db::parse_drv;
use gradient_core::nix::DerivationResolver;
use proto::messages::{DerivationOutput, DiscoveredDerivation, FlakeJob};
use tracing::{debug, warn};

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

/// Advance status to `EvaluatingFlake`.
///
/// Attr discovery is done inside [`evaluate_derivations`] since the server
/// only cares about the final [`DiscoveredDerivation`] list.
pub async fn evaluate_flake(_job: &FlakeJob, updater: &mut JobUpdater<'_>) -> Result<()> {
    updater.report_evaluating_flake().await
}

/// Walk the full derivation closure and report [`DiscoveredDerivation`]s to
/// the server.
///
/// This is the main evaluation step:
/// 1. Discover attr paths (via eval worker pool)
/// 2. Resolve attrs to .drv paths (via eval worker pool)
/// 3. BFS from root .drv paths through `inputDrvs` references
/// 4. For each .drv: read file, extract outputs/arch/features
/// 5. Query server cache, mark substituted derivations
/// 6. Send `EvalResult` with the full derivation set
pub async fn evaluate_derivations(
    evaluator: &WorkerEvaluator,
    job: &FlakeJob,
    local_flake_path: Option<&str>,
    updater: &mut JobUpdater<'_>,
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
    // Prefer `git+file://` over `path:` for local clones: `path:` puts Nix
    // into impure evaluation mode, which lets `builtins.fetchGit` (and similar
    // builtins) attempt live network fetches.  Those IO failures are NOT
    // catchable by `builtins.tryEval`, so a single module with an un-pinned
    // `fetchGit` call crashes the whole evaluation.  `git+file://?rev=` keeps
    // Nix in pure mode: `builtins.fetchGit` without a `rev` then throws a
    // regular exception that `tryEval` can handle gracefully.
    let repo = if let Some(path) = local_flake_path {
        format!("git+file://{}?rev={}", path, job.commit)
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

    // ── Step 3+4: BFS closure walk ────────────────────────────────────────────
    // Start from all root drv paths. For each node: read the .drv file to get
    // inputs, outputs, architecture, features; then enqueue inputs.
    let mut discovered: Vec<DiscoveredDerivation> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(Option<String>, String)> = VecDeque::new(); // (attr, drv_path)

    for (attr, drv) in &root_drvs {
        if visited.insert(drv.clone()) {
            queue.push_back((Some(attr.clone()), drv.clone()));
        }
    }

    while let Some((attr, drv_path)) = queue.pop_front() {
        let drv_bytes = match drv_reader.read_drv(&drv_path).await {
            Ok(b) => b,
            Err(e) => {
                warn!(drv = %drv_path, error = %e, "failed to read .drv file; skipping");
                warnings.push(format!("cannot read {drv_path}: {e}"));
                continue;
            }
        };

        let drv = match parse_drv(&drv_bytes) {
            Ok(d) => d,
            Err(e) => {
                warn!(drv = %drv_path, error = %e, "failed to parse .drv file; skipping");
                warnings.push(format!("cannot parse {drv_path}: {e}"));
                continue;
            }
        };

        // Enqueue input derivations (BFS).
        for (input_drv, _outputs) in &drv.input_derivations {
            if visited.insert(input_drv.clone()) {
                queue.push_back((None, input_drv.clone()));
            }
        }

        // Map outputs.
        let outputs: Vec<DerivationOutput> = drv
            .outputs
            .iter()
            .filter(|o| !o.path.is_empty())
            .map(|o| DerivationOutput {
                name: o.name.clone(),
                path: o.path.clone(),
            })
            .collect();

        let architecture = drv.system.clone();

        // Collect dependencies (just the drv paths, no output info needed here).
        let dependencies: Vec<String> = drv
            .input_derivations
            .iter()
            .map(|(p, _)| p.clone())
            .collect();

        let required_features = drv.required_system_features();

        discovered.push(DiscoveredDerivation {
            attr: attr.unwrap_or_default(),
            drv_path: drv_path.clone(),
            outputs,
            dependencies,
            architecture,
            required_features,
            substituted: false, // set after cache query below
        });
    }

    // ── Step 5: query server cache ────────────────────────────────────────
    // Collect all output paths and query the server's cache to determine
    // which derivations are already available (substituted).
    let all_output_paths: Vec<String> = discovered
        .iter()
        .flat_map(|d| d.outputs.iter().map(|o| o.path.clone()))
        .collect();

    if !all_output_paths.is_empty() {
        // Ask the server which outputs are available — local cache (url: None)
        // or upstream external caches (url: Some). Both are treated as substituted.
        let cached_paths = updater
            .query_cache(all_output_paths.clone())
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "cache query failed; treating all paths as uncached");
                vec![]
            });

        let cached_set: HashSet<&str> = cached_paths.iter().map(|c| c.path.as_str()).collect();

        // Mark derivations whose outputs are all in the server's cache.
        for drv in &mut discovered {
            if !drv.outputs.is_empty()
                && drv
                    .outputs
                    .iter()
                    .all(|o| cached_set.contains(o.path.as_str()))
            {
                drv.substituted = true;
            }
        }

        debug!(
            total = discovered.len(),
            substituted = discovered.iter().filter(|d| d.substituted).count(),
            "cache query complete"
        );
    }

    warnings.sort_unstable();
    warnings.dedup();
    errors.sort_unstable();
    errors.dedup();

    debug!(
        discovered = discovered.len(),
        warnings = warnings.len(),
        errors = errors.len(),
        "closure walk complete"
    );

    updater.report_eval_result(discovered, warnings, errors).await
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

        // Should have EvaluatingDerivations + EvalResult events.
        assert_eq!(reporter.len(), 2);

        if let ReportedEvent::EvalResult {
            derivations,
            warnings,
        } = reporter.last_eval_result().unwrap()
        {
            // All derivations from the fixture should be discovered.
            assert_eq!(derivations.len(), fixture.derivations.len());
            // Nothing is built → nothing substituted.
            assert!(derivations.iter().all(|d| !d.substituted));
            // Entry point should have the attr set.
            let entry = derivations
                .iter()
                .find(|d| d.drv_path == fixture.entry_point)
                .unwrap();
            assert_eq!(entry.attr, "hello");
            // Warnings should be empty for a valid fixture.
            assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);
        } else {
            panic!("expected EvalResult");
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

        if let ReportedEvent::EvalResult { derivations, .. } = reporter.last_eval_result().unwrap()
        {
            let substituted_count = derivations.iter().filter(|d| d.substituted).count();
            let not_substituted = derivations.iter().filter(|d| !d.substituted).count();
            assert!(substituted_count > 0, "some should be substituted");
            assert!(not_substituted > 0, "some should not be substituted");

            // Verify substituted flags match the fixture's built state.
            for drv in derivations {
                let is_built = fixture.built().iter().any(|b| b.drv_path == drv.drv_path);
                assert_eq!(
                    drv.substituted, is_built,
                    "substituted mismatch for {}: got {} expected {}",
                    drv.drv_path, drv.substituted, is_built
                );
            }
        } else {
            panic!("expected EvalResult");
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

        if let ReportedEvent::EvalResult { derivations, .. } = reporter.last_eval_result().unwrap()
        {
            assert!(
                derivations.iter().all(|d| d.substituted),
                "all should be substituted when everything is cached"
            );
        } else {
            panic!("expected EvalResult");
        }
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
    async fn test_eval_missing_drv_warns() {
        // Resolver resolves an attr to a drv path that doesn't exist in the reader.
        let resolver = FakeDerivationResolver::new()
            .with_flake_attrs("repo", vec!["pkg".into()])
            .with_drv_path("repo", "pkg", "/nix/store/nonexistent.drv");
        let drv_reader = FakeDrvReader::new(); // No drvs loaded
        let job = make_flake_job("repo");
        let mut reporter = RecordingJobReporter::new();

        evaluate_derivations_with(&resolver, &drv_reader, &job, None, &mut reporter)
            .await
            .unwrap();

        if let ReportedEvent::EvalResult {
            derivations,
            warnings,
        } = reporter.last_eval_result().unwrap()
        {
            assert!(
                derivations.is_empty(),
                "no derivations should be discovered"
            );
            assert!(
                !warnings.is_empty(),
                "should have a warning about missing drv"
            );
            assert!(
                warnings[0].contains("nonexistent.drv"),
                "warning should mention the missing path: {:?}",
                warnings
            );
        } else {
            panic!("expected EvalResult");
        }
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

        if let ReportedEvent::EvalResult { derivations, .. } = reporter.last_eval_result().unwrap()
        {
            // Build a dependency map from the eval result.
            let eval_deps: std::collections::HashMap<&str, Vec<&str>> = derivations
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
        } else {
            panic!("expected EvalResult");
        }
    }
}
