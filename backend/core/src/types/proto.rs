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
    /// When set, the worker signs all fetched store paths after pushing their
    /// NARs.  Requires a `SigningKey` credential to be delivered before the job.
    pub sign: Option<SignTask>,
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

/// A store path and its Ed25519 narinfo signature.
///
/// `signature` is in the standard Nix format `key-name:base64` as returned by
/// `harmonia_store_core::signature::Signature::to_string()`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct PathSignature {
    pub store_path: String,
    pub signature: String,
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
    /// Worker has finished signing store paths and reports each signature so
    /// the server can record them in `cached_path_signature`.
    Signed {
        signatures: Vec<PathSignature>,
    },
}

/// A flake input fetched during the `FetchFlake` task.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FetchedInput {
    pub store_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    /// Ed25519 signature produced by the `sign` step, if present.
    /// Format: `<key-name>:<base64>` — the standard Nix narinfo signature format.
    pub signature: Option<String>,
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

/// Query mode for [`CacheQuery`].
///
/// Controls what the server returns in [`CacheStatus`] beyond the basic
/// cached/uncached flag.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub enum QueryMode {
    /// Return only paths that are already in the cache (`cached: true`).
    /// No presigned URLs are generated.  This is the default and is used
    /// during evaluation to determine which derivations are substituted.
    #[default]
    Normal,
    /// Return cached paths with a presigned S3 GET URL in `url`.
    /// When `url` is `None`, the worker should download via `NarRequest`.
    /// Used by build workers to fetch required store paths.
    Pull,
    /// Return **all** queried paths.  Uncached paths include a presigned S3
    /// PUT URL in `url` so the worker can upload directly to S3.
    /// When `url` is `None` for an uncached path, the worker should upload via
    /// `NarPush`.  Cached paths have `cached: true` and no URL (skip them).
    /// Used after `FetchFlake` to push fetched inputs to the server cache.
    Push,
}

/// A store path entry returned in [`CacheStatus`].
///
/// `cached` indicates whether the path is already in the Gradient cache.
/// `url` provides a presigned S3 URL (GET for [`QueryMode::Pull`], PUT for
/// [`QueryMode::Push`]); `None` means use the direct WebSocket transfer
/// (`NarRequest` / `NarPush`) instead.
///
/// In [`QueryMode::Pull`] (worker fetching dep NARs into its local store), the
/// `nar_hash` / `references` / `signatures` / `deriver` / `ca` fields carry the
/// path info the worker needs to construct a `ValidPathInfo` and call
/// `add_to_store_nar` on its local nix-daemon. They are `None` for other
/// modes and for uncached paths.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CachedPath {
    pub path: String,
    /// `true` if the path is present in the Gradient cache (local or upstream).
    pub cached: bool,
    /// Compressed NAR size on disk (bytes). `None` if not yet recorded.
    pub file_size: Option<u64>,
    /// Uncompressed NAR size (bytes). `None` if not yet recorded.
    pub nar_size: Option<u64>,
    /// Presigned S3 URL for direct transfer.
    ///
    /// - [`QueryMode::Pull`]: GET URL to download the NAR from S3.
    /// - [`QueryMode::Push`]: PUT URL to upload the NAR to S3 (only set when
    ///   `cached` is `false`).
    /// - `None`: use WebSocket direct transfer (`NarRequest` or `NarPush`).
    pub url: Option<String>,
    /// SHA-256 of the uncompressed NAR in `sha256:<nix32>` format.
    /// Populated for cached paths in [`QueryMode::Pull`].
    pub nar_hash: Option<String>,
    /// Other store paths this path references (full `/nix/store/...` paths).
    /// Populated for cached paths in [`QueryMode::Pull`].
    pub references: Option<Vec<String>>,
    /// Cache signatures in narinfo wire format `<key-name>:<base64>`.
    /// Populated for cached paths in [`QueryMode::Pull`].
    pub signatures: Option<Vec<String>>,
    /// Optional deriver: the `.drv` path that produced this output (full path).
    /// Populated for cached paths in [`QueryMode::Pull`] when known.
    pub deriver: Option<String>,
    /// Content-addressed identifier (e.g. `fixed:r:sha256:<hash>` for FOD).
    /// Populated for cached paths in [`QueryMode::Pull`] when the path is CA.
    pub ca: Option<String>,
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

/// Discriminates between the two schedulable job kinds.
///
/// Used in [`RequestJob`] to let the worker signal capacity per job type.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum JobKind {
    /// Flake evaluation job (fetch / eval).
    Flake,
    /// Nix build job.
    Build,
}
