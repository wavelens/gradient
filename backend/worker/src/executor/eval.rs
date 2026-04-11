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

use anyhow::{Context, Result};
use evaluator::WorkerPoolResolver;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_core::db::parse_drv;
use gradient_core::nix::DerivationResolver;
use proto::messages::{Architecture as ProtoArch, DerivationOutput, DiscoveredDerivation, FlakeJob};
use tracing::{debug, warn};

use crate::job::JobUpdater;
use crate::store::LocalNixStore;

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
/// 5. Check local store for substitution status
/// 6. Send `EvalResult` with the full derivation set
pub async fn evaluate_derivations(
    evaluator: &WorkerEvaluator,
    store: &LocalNixStore,
    job: &FlakeJob,
    updater: &mut JobUpdater<'_>,
) -> Result<()> {
    updater.report_evaluating_derivations().await?;

    let repo = job.repository.clone();
    let wildcards = job.wildcards.clone();

    // ── Step 1: discover attr paths ──────────────────────────────────────────
    debug!(repo = %repo, "listing flake derivations");
    let (attrs, mut warnings) = evaluator
        .resolver
        .list_flake_derivations(repo.clone(), wildcards)
        .await
        .context("list_flake_derivations failed")?;

    if attrs.is_empty() {
        warn!("no derivations found for evaluation");
        updater.report_eval_result(vec![], warnings).await?;
        return Ok(());
    }

    // ── Step 2: resolve attr paths → drv paths ───────────────────────────────
    let (resolved, resolve_warnings) = evaluator
        .resolver
        .resolve_derivation_paths(repo.clone(), attrs.clone())
        .await
        .context("resolve_derivation_paths failed")?;
    warnings.extend(resolve_warnings);

    // Build (attr, drv_path) pairs from successful resolutions.
    let mut root_drvs: Vec<(String, String)> = Vec::new();
    for (attr, result) in resolved {
        match result {
            Ok((drv_path, _refs)) => root_drvs.push((attr, drv_path)),
            Err(e) => {
                warn!(attr, error = %e, "failed to resolve attr; skipping");
                warnings.push(format!("failed to resolve {attr}: {e}"));
            }
        }
    }

    if root_drvs.is_empty() {
        warn!("all attr resolutions failed");
        updater.report_eval_result(vec![], warnings).await?;
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
        let full_path = if drv_path.starts_with('/') {
            drv_path.clone()
        } else {
            format!("/nix/store/{}", drv_path)
        };

        let drv_bytes = match tokio::fs::read(&full_path).await {
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

        // Check if all outputs are already in the local store.
        let substituted = if outputs.is_empty() {
            false
        } else {
            let checks: FuturesUnordered<_> = outputs
                .iter()
                .map(|o| store.has_path(&o.path))
                .collect();
            let results: Vec<_> = checks.collect().await;
            results.into_iter().all(|r| r.unwrap_or(false))
        };

        // Map system string → proto Architecture.
        let architecture = system_to_proto_arch(&drv.system);

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
            substituted,
        });
    }

    warnings.sort_unstable();
    warnings.dedup();

    debug!(
        discovered = discovered.len(),
        warnings = warnings.len(),
        "closure walk complete"
    );

    updater.report_eval_result(discovered, warnings).await
}

/// Convert a Nix system string to the proto [`Architecture`] enum.
fn system_to_proto_arch(system: &str) -> ProtoArch {
    match system {
        "x86_64-linux" => ProtoArch::X86_64Linux,
        "aarch64-linux" => ProtoArch::Aarch64Linux,
        "x86_64-darwin" => ProtoArch::X86_64Darwin,
        "aarch64-darwin" => ProtoArch::Aarch64Darwin,
        _ => ProtoArch::Builtin,
    }
}
