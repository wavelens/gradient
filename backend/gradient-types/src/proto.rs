/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Feature flags exchanged during the protocol handshake.
///
/// Each field represents one optional capability.  The client sends the flags
/// it supports in `ClientMessage::InitConnection`; the server responds with
/// only the flags it is willing to activate for this session in
/// `ServerMessage::InitAck`.  Unknown flags in a received message are always
/// treated as `false` - adding new fields is forwards-compatible.
///
/// All fields default to `false` so a zeroed struct is a valid
/// "no features" state.
#[derive(
    Archive,
    Serialize,
    Deserialize,
    SerdeSerialize,
    SerdeDeserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[rkyv(derive(Debug, PartialEq))]
pub struct GradientCapabilities {
    /// Peer is the Gradient server itself (coordinator).
    /// Always `true` on the server side, always `false` for external workers.
    pub core: bool,
    /// Client supports federation - relaying work and NAR traffic between workers and servers.
    pub federate: bool,
    /// Client supports fetching flake inputs and pre-fetching sources.
    pub fetch: bool,
    /// Client supports Nix flake evaluation.
    pub eval: bool,
    /// Client supports executing Nix builds.
    pub build: bool,
    /// Peer serves as a Nix binary cache. Always advertised by the server.
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

/// Where to obtain the flake source for a [`FlakeJob`].
///
/// `Repository` requires the worker to have the `fetch` capability and the
/// `FetchFlake` task in `tasks`. `Cached` is used for eval-only follow-up
/// jobs dispatched after a fetch-capable worker has already archived the
/// source into the cache.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum FlakeSource {
    Repository { url: String, commit: String },
    Cached { store_path: String },
}

/// Per-input override applied during `FetchFlake`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FlakeInputOverride {
    pub input_name: String,
    /// `None` keeps the URL from the project's flake but still forces an update.
    pub url: Option<String>,
}

/// Drives an `input_update` evaluation: which generator to run and which flake
/// inputs to bump during `FetchFlake`. `None` on a normal evaluation.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct InputUpdateSpec {
    /// `PatchGeneratorKind` snake_case tag, e.g. `flake_lock`.
    pub generator: String,
    /// Tracked input names to bump. Empty means "all tracked inputs".
    pub inputs: Vec<String>,
}

/// One bumped input, reported back from the worker for the sidecar + PR body.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BumpedInputWire {
    pub name: String,
    pub old_rev: Option<String>,
    pub new_rev: String,
}

/// Evaluation job: fetch and/or evaluate a Nix flake.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FlakeJob {
    pub tasks: Vec<FlakeTask>,
    pub source: FlakeSource,
    pub wildcards: Vec<String>,
    pub timeout_secs: Option<u64>,
    /// Per-input overrides applied during `FetchFlake`. Empty means no overrides.
    pub input_overrides: Vec<FlakeInputOverride>,
    /// Set on an `input_update` evaluation to bump tracked inputs during fetch.
    pub input_update: Option<InputUpdateSpec>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum FlakeTask {
    FetchFlake,
    EvaluateFlake,
    EvaluateDerivations,
}

/// Build job: build derivations. The worker always zstd-compresses
/// uploaded NARs and reports NAR metadata in `NarUploaded`; the server
/// computes and stores narinfo signatures from that metadata.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildJob {
    pub builds: Vec<BuildTask>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildTask {
    pub build_id: String,
    pub drv_path: String,
    /// When `true` the build's outputs are known to be available from an
    /// upstream cache (cache.nixos.org etc.) but are not yet in the
    /// gradient cache. Worker behavior changes: instead of running
    /// `nix build`, the worker re-queries the cache (`CacheQuery Pull`)
    /// to get the upstream URL for each output, downloads the NAR,
    /// recompresses to zstd, and pushes via `NarUploaded`. No daemon
    /// build invocation, no input prefetch.
    pub external_cached: bool,
    /// Fixed-output (content-addressed) derivation. Carried for worker-side
    /// scoring; substitution itself is attempted for every build regardless.
    pub is_fixed_output: bool,
    /// Output `(name, store_path)` pairs, populated only for `external_cached`
    /// substitutions. The worker fetches these outputs directly instead of the
    /// `.drv`: a substitution needs only the output NAR plus its runtime
    /// closure, never the `.drv`'s build-time `input_sources` (which binary
    /// caches do not serve, so importing the `.drv` would spuriously fail with
    /// `SubstituteUnavailable`). Empty for normal builds.
    pub outputs: Vec<DerivationOutput>,
    /// Wall-clock limit in seconds for this build; `None` = no limit.
    pub timeout_secs: Option<u64>,
    /// Silent (no-output) limit in seconds; `None` = no limit.
    pub max_silent_secs: Option<u64>,
}

/// Severity of a worker-reported evaluation message. Mirrors
/// `gradient_entity::evaluation_message::MessageLevel` on the wire.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum EvalMessageLevel {
    Error,
    Warning,
    Notice,
}

/// Progress events for job updates.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum JobUpdateKind {
    Fetching,
    /// Reports the archived flake source path after `FetchFlake` completes.
    /// `Some(path)` - `nix flake archive` succeeded and the source now lives
    /// in the cache; the server can hand the path to a subsequent eval-only
    /// job as `FlakeSource::Cached`. `None` - the worker fell back to a
    /// temporary git checkout; no eval-only follow-up is possible.
    FetchResult {
        flake_source: Option<String>,
    },
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        /// Nix stderr warnings (deprecations, etc.) - informational.
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
        /// Per-build resource usage for this build; `None` when capture is
        /// disabled. A multi-build job yields one `BuildOutput` (and thus one
        /// metrics record) per build.
        metrics: Option<BuildMetrics>,
        /// True when the daemon reported the outputs as already valid (no work
        /// performed) - the build is finalized as `Substituted`, not `Completed`.
        substituted: bool,
    },
    Compressing,
    /// Per-evaluation stats + walked flake-output graph, sent once at eval
    /// completion. Informational/metrics only - does not affect job state.
    EvalStats(EvalStatsReport),
    /// Worker-produced candidate `flake.lock` (utf-8) and the inputs it bumped,
    /// reported during `FetchFlake` of an `input_update` eval. Empty `bumped`
    /// means nothing changed and no PR should be opened.
    InputUpdateResult {
        candidate_lock: String,
        bumped: Vec<BumpedInputWire>,
    },
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

/// Result of an eval-cache pull request (`EvalCachePull`).
///
/// Mirrors the NAR pull modes: the server either has no cached blob for the
/// fingerprint (`Miss`), serves it via a presigned S3 GET URL (`Presigned` -
/// the worker does the HTTP transfer itself), or streams the blob inline over
/// the proto channel as `EvalCacheChunk` frames (`Inline`, local-FS fallback).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum EvalCachePullOutcome {
    /// No cached eval-cache blob for this fingerprint.
    Miss,
    /// Presigned S3 GET URL; the worker downloads the blob directly.
    Presigned { url: String },
    /// The server will stream the blob inline as `EvalCacheChunk` frames.
    /// `stream_token` guards the chunk stream like the NAR transfer token.
    Inline { total_bytes: u64, stream_token: String },
}

/// Result of an eval-cache push grant (`EvalCachePushGrant`).
///
/// Mirrors the NAR push modes: the server already holds the blob (`Skip`),
/// grants a presigned S3 PUT URL (`Presigned` - the worker uploads then sends
/// `EvalCachePushDone`), or accepts an inline upload of `EvalCacheChunk` frames
/// (`Inline`, local-FS fallback).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum EvalCachePushMode {
    /// Server already has this fingerprint; the worker uploads nothing.
    Skip,
    /// Presigned S3 PUT URL; the worker uploads then sends `EvalCachePushDone`.
    Presigned { url: String },
    /// The worker should stream the blob inline as `EvalCacheChunk` frames.
    /// `stream_token` guards the chunk stream like the NAR transfer token.
    Inline { stream_token: String },
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
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub allow_substitutes: bool,
    pub pname: Option<String>,
    pub substituted: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
}

/// One product declared in a build output's `nix-support/hydra-build-products`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildProduct {
    /// Hydra product type, e.g. "file", "doc", "report".
    pub file_type: String,
    /// Hydra product subtype (the second token), e.g. "readme", "html", "binary-dist".
    pub subtype: String,
    /// Basename of `path`.
    pub name: String,
    /// Absolute store path to the product file (e.g. `/nix/store/abc-pkg/image.iso`).
    pub path: String,
    /// Product file size in bytes (from `stat`); `None` on stat failure.
    pub size: Option<u64>,
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
    pub products: Vec<BuildProduct>,
}

/// Per-build resource usage, captured by the worker from the build's cgroup
/// (best-effort). `build_time_ms` is always present; cgroup-derived fields
/// degrade to `None` when the cgroup cannot be located or read.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildMetrics {
    pub peak_ram_mb: Option<u64>,
    pub cpu_time_ms: Option<u64>,
    pub avg_cpu_pct: Option<f32>,
    pub disk_read_bytes: Option<u64>,
    pub disk_write_bytes: Option<u64>,
    pub oom_killed: bool,
    pub build_time_ms: Option<u64>,
    /// Host network throughput peak (Mbps) observed during the build window.
    /// Host-level, not cgroup-attributed (cgroup v2 has no per-build network);
    /// accurate when the build is the host's sole network consumer.
    pub peak_network_mbps: Option<f32>,
}

/// Per-entry-point evaluation cost, aggregated by the worker across the
/// requests resolving that entry point's attributes.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct EvalAttrCost {
    pub attr: String,
    pub thunks: u64,
    pub fn_calls: u64,
    pub eval_ms: u64,
    pub alloc_bytes: u64,
}

/// One node of the flake-output graph actually walked during discovery
/// (no extra evaluation). `parent`/`drv_path` are `None` at the root / for
/// non-derivation nodes.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FlakeOutputNode {
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

/// Per-evaluation statistics + walked flake-output graph, sent once at eval
/// completion. Byte gauges are pre-converted to MB by the worker.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub struct EvalStatsReport {
    pub total_thunks: u64,
    pub fn_calls: u64,
    pub primop_calls: u64,
    pub lookups: u64,
    pub alloc_bytes: u64,
    pub peak_heap_mb: u64,
    pub peak_rss_mb: u64,
    pub fetch_ms: u64,
    pub eval_flake_ms: u64,
    pub eval_drv_ms: u64,
    pub total_eval_ms: u64,
    pub worker_id: String,
    pub per_entry_point: Vec<EvalAttrCost>,
    pub flake_nodes: Vec<FlakeOutputNode>,
}

// ── Credential types ─────────────────────────────────────────────────────────

/// Type of credential delivered via the protocol.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum CredentialKind {
    SshKey,
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

/// Why a build failed, as classified by the worker. Drives the scheduler's
/// retry decision.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[rkyv(derive(Debug, PartialEq))]
pub enum BuildFailureKind {
    /// Infrastructure failure (OOM, disk full, network/substitution error,
    /// builder crash) - eligible for retry.
    Transient,
    /// The builder exited non-zero, or the retry budget is exhausted -
    /// terminal. Default so eval-job failures (which ignore the kind) decode
    /// to a safe terminal value.
    #[default]
    Permanent,
    /// Wall-clock or silent timeout exceeded - terminal.
    Timeout,
    /// A substitute attempt could not pull the output from cache. Penalty-free
    /// re-queue; escalates to a real arch-bound build after repeated misses.
    SubstituteUnavailable,
    /// Prefetch found required input paths that the gradient cache cannot
    /// serve: a transitive dependency is marked done/substituted yet its NAR is
    /// absent. Terminal for this build; the server demotes those outputs and
    /// re-queues their producers so the next evaluation succeeds. Carries the
    /// offending paths on `ClientMessage::JobFailed.missing_paths`.
    InputsUnavailable,
}
