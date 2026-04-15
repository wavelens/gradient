/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize, Serialize};
use serde::Serialize as SerdeSerialize;

/// Feature flags exchanged during the protocol handshake.
///
/// Each field represents one optional capability.  The client sends the flags
/// it supports in `ClientMessage::InitConnection`; the server responds with
/// only the flags it is willing to activate for this session in
/// `ServerMessage::InitAck`.  Unknown flags in a received message are always
/// treated as `false` — adding new fields is forwards-compatible.
///
/// All fields default to `false` so a zeroed struct is a valid
/// "no features" state.
#[derive(Archive, Serialize, Deserialize, SerdeSerialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct GradientCapabilities {
    /// Peer is the Gradient server itself (coordinator).
    /// Always `true` on the server side, always `false` for external workers.
    pub core: bool,
    /// Client supports federation — relaying work and NAR traffic between workers and servers.
    pub federate: bool,
    /// Client supports fetching flake inputs and pre-fetching sources.
    pub fetch: bool,
    /// Client supports Nix flake evaluation.
    pub eval: bool,
    /// Client supports executing Nix builds.
    pub build: bool,
    /// Client supports signing store paths and uploading signatures.
    pub sign: bool,
    /// Peer serves as a Nix binary cache.
    /// Set by the server when `GRADIENT_SERVE_CACHE=true`, never by workers.
    pub cache: bool,
}

// ── Job types ────────────────────────────────────────────────────────────────

/// A job is an ordered sequence of tasks.  If any task fails, the rest are
/// skipped and the job is reported as failed.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum Job {
    Flake(FlakeJob),
    Build(BuildJob),
}

/// Evaluation job: fetch and/or evaluate a Nix flake.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FlakeJob {
    pub tasks: Vec<FlakeTask>,
    pub repository: String,
    pub commit: String,
    pub wildcards: Vec<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum FlakeTask {
    FetchFlake,
    EvaluateFlake,
    EvaluateDerivations,
}

/// Build job: build derivations, optionally compress and sign outputs.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildJob {
    pub builds: Vec<BuildTask>,
    pub compress: Option<CompressTask>,
    pub sign: Option<SignTask>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildTask {
    pub build_id: String,
    pub drv_path: String,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CompressTask {
    pub store_paths: Vec<String>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct SignTask {
    pub store_paths: Vec<String>,
}

/// Progress events for job updates.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum JobUpdateKind {
    Fetching,
    FetchResult {
        fetched_paths: Vec<FetchedInput>,
    },
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        /// Nix stderr warnings (deprecations, etc.) — informational.
        warnings: Vec<String>,
        /// Hard errors that prevented derivation resolution (e.g. per-attr
        /// `.drvPath` evaluation failures).  A non-empty list should cause
        /// the evaluation to be marked `Failed` server-side.
        errors: Vec<String>,
    },
    Building {
        build_id: String,
    },
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
    },
    Compressing,
    Signing,
}

/// A flake input fetched during the `FetchFlake` task.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FetchedInput {
    pub store_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
}

// ── Scheduling types ─────────────────────────────────────────────────────────

/// Cache metadata for a store path.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CacheInfo {
    /// Compressed NAR size on disk (bytes).
    pub file_size: u64,
    /// Uncompressed NAR size (bytes).
    pub nar_size: u64,
}

/// A store path available to the worker, returned in [`CacheStatus`].
///
/// When `url` is `None` the path is in the local Gradient cache — the worker
/// fetches it via `NarRequest` / `PresignedDownload` as usual.
///
/// When `url` is `Some` the path was found in an upstream external Nix binary
/// cache; the worker downloads the NAR directly from that absolute URL, then
/// compresses and signs it before uploading to the Gradient cache.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CachedPath {
    pub path: String,
    /// Compressed NAR size on disk (bytes). `None` if not yet recorded.
    pub file_size: Option<u64>,
    /// Uncompressed NAR size (bytes). `None` if not yet recorded.
    pub nar_size: Option<u64>,
    /// Absolute NAR URL for upstream paths; `None` for local Gradient cache.
    pub url: Option<String>,
}

/// A store path required by a job candidate, with optional cache metadata.
///
/// `cache_info` is `Some` when the path is known to be in the server's binary
/// cache, allowing workers to estimate download cost during scoring.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct RequiredPath {
    pub path: String,
    pub cache_info: Option<CacheInfo>,
}

/// A job candidate pushed to workers by the server.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct JobCandidate {
    pub job_id: String,
    pub required_paths: Vec<RequiredPath>,
    /// Derivation paths for build candidates; empty for eval jobs.
    /// Workers use these to read the `.drv` file and determine the
    /// actual set of inputs needed.
    pub drv_paths: Vec<String>,
}

/// A worker's score for a single job candidate.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CandidateScore {
    pub job_id: String,
    /// Number of required paths not present in the worker's local Nix store.
    pub missing_count: u32,
    /// Total uncompressed NAR size of missing paths (bytes).
    /// Derived from `CacheInfo.nar_size`; zero when cache info is unavailable.
    pub missing_nar_size: u64,
}

// ── Derivation discovery ─────────────────────────────────────────────────────

/// A derivation discovered during evaluation.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct DiscoveredDerivation {
    pub attr: String,
    pub drv_path: String,
    pub outputs: Vec<DerivationOutput>,
    pub dependencies: Vec<String>,
    pub architecture: String,
    pub required_features: Vec<String>,
    pub substituted: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
}

/// Build output reported after a derivation successfully builds.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildOutput {
    pub name: String,
    pub store_path: String,
    pub hash: String,
    pub nar_size: Option<i64>,
    pub nar_hash: Option<String>,
    pub has_artefacts: bool,
}

// ── Credential types ─────────────────────────────────────────────────────────

/// Type of credential delivered via the protocol.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum CredentialKind {
    SshKey,
    SigningKey,
}
