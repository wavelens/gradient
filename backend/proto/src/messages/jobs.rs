/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize, Serialize};

use super::types::{BuildOutput, DiscoveredDerivation};

// ── Job ───────────────────────────────────────────────────────────────────────

/// A job is an ordered sequence of tasks.  If any task fails, the rest are
/// skipped and [`super::client::ClientMessage::JobFailed`] is sent.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum Job {
    Flake(FlakeJob),
    Build(BuildJob),
}

// ── FlakeJob ──────────────────────────────────────────────────────────────────

/// Evaluation job: fetch and/or evaluate a Nix flake.
///
/// The server includes only the tasks matching the worker's negotiated
/// capabilities (`fetch`, `eval`).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FlakeJob {
    /// Subset of tasks to execute, in order.
    pub tasks: Vec<FlakeTask>,
    /// Git repository URL (used by `FetchFlake`).
    pub repository: String,
    /// Commit SHA to check out.
    pub commit: String,
    /// Attribute wildcard patterns (used by `EvaluateFlake`).
    pub wildcards: Vec<String>,
    /// Evaluation timeout in seconds (`None` = server default).
    pub timeout_secs: Option<u64>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum FlakeTask {
    /// Clone the repository and check out the commit.
    /// Requires `fetch` capability; SSH key sent via
    /// [`super::server::ServerMessage::Credential`].
    FetchFlake,
    /// Run `nix eval` to discover attribute paths matching `wildcards`.
    /// Requires `eval` capability.
    EvaluateFlake,
    /// Walk the derivation closure (BFS) and report
    /// [`JobUpdateKind::EvalResult`] batches incrementally.
    /// Requires `eval` capability.
    EvaluateDerivations,
}

// ── BuildJob ──────────────────────────────────────────────────────────────────

/// Build job: build a chain of derivations, compress outputs into NARs,
/// and optionally sign the results.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildJob {
    /// Derivations to build, in topological order (dependencies first).
    pub builds: Vec<BuildTask>,
    /// Optional compression step.  Packs build outputs into zstd-compressed
    /// NARs for upload.  Runs after all builds complete, before signing.
    pub compress: Option<CompressTask>,
    /// Optional signing step.  Requires `sign` capability.
    pub sign: Option<SignTask>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildTask {
    /// DB `build` row UUID — used in [`JobUpdateKind::Building`] and
    /// [`JobUpdateKind::BuildOutput`].
    pub build_id: String,
    /// Path to the `.drv` file in the Nix store.
    pub drv_path: String,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CompressTask {
    /// Store paths to pack into zstd-compressed NARs.
    pub store_paths: Vec<String>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct SignTask {
    /// Store paths to sign with the cache signing key.
    pub store_paths: Vec<String>,
}

// ── Progress updates ──────────────────────────────────────────────────────────

/// Granular progress events sent via
/// [`super::client::ClientMessage::JobUpdate`].
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum JobUpdateKind {
    // ── FlakeJob phases → maps to EvaluationStatus ───────────────────────────
    /// Cloning the repository.  → `EvaluationStatus::Fetching`
    Fetching,
    /// Running `nix eval` to find attributes.  → `EvaluationStatus::EvaluatingFlake`
    EvaluatingFlake,
    /// Walking the derivation closure.  → `EvaluationStatus::EvaluatingDerivation`
    EvaluatingDerivations,
    /// Incremental batch of discovered derivations.  May be sent many times.
    /// First batch sets the evaluation to `Building`.
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        /// Nix evaluation warnings captured from stderr.
        warnings: Vec<String>,
    },

    // ── BuildJob phases → maps to BuildStatus ────────────────────────────────
    /// Starting to build a derivation.  → `BuildStatus::Building`
    Building { build_id: String },
    /// A derivation finished building; outputs are ready.  → `BuildStatus::Completed`
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
    },
    /// Compressing build outputs into zstd NARs for upload.
    Compressing,
    /// Signing outputs with the cache key (informational, no DB status change).
    Signing,
}
