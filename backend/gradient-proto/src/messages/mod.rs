/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod client;
pub mod server;

// Job and scheduling types live in gradient_types::proto - re-exported here for
// backward compatibility so existing `crate::messages::FlakeJob` paths still work.
pub use client::{ArchivedClientMessage, ClientMessage};
pub use gradient_types::proto::{
    BuildFailureKind, BuildJob, BuildMetrics, BuildOutput, BuildProduct, BuildSpec,
    BumpedInputWire, CacheInfo, CachedPath, CandidateScore, CredentialKind, DerivationOutput,
    DiscoveredDerivation, EvalAttrCost, EvalCachePullOutcome, EvalCachePushMode, EvalMessageLevel,
    EvalStatsReport, FlakeInputOverride, FlakeJob, FlakeOutputNode, FlakeSource, FlakeStep,
    GradientCapabilities, InputUpdateSpec, Job, JobCandidate, JobKind, JobPhase, JobPhaseSpan,
    JobUpdateKind, QueryMode, RequiredPath,
};
pub use server::{ArchivedServerMessage, FailedPeer, ServerMessage};

/// Wire protocol version implemented by this build.
/// v5: dropped `PresignedUpload`/`PresignedDownload` and `AssignJob.timeout_secs`.
/// v7: `CacheQuery`/`CacheStatus`/`CacheError` carry a per-query `query_id`;
///     `NarUploaded` carries the path's content address (`ca`).
/// v8: `BuildFailureKind::Aborted` distinguishes a server-ordered abort from a
///     deterministic build failure.
/// v9: `JobCompleted`/`JobFailed` carry the worker's phase timeline (`spans`);
///     `EvalStatsReport` drops the three phase-millisecond fields it never set.
/// v10: `QueryMode::PullClosure` asks the server to answer for a path's whole
///      reference closure, not just the path.
/// v11: `AssignJob` carries the `dispatched_job` id; `JobUpdate`, `JobCompleted`
///      and `JobFailed` echo it and a report from another dispatch is dropped.
///      `DiscoveredDerivation` drops `substituted`; a pruned dependency is no
///      longer reported as an entry of its own.
/// v12: rkyv archives are unaligned and read in place; `QueryKnownDerivations`
///      carries a `query_id` that `KnownDerivations` echoes; bulk chunks are
///      512 KiB and a bulk write batch is byte-capped.
pub const PROTO_VERSION: u16 = 12;

pub use gradient_types::constants::{NAR_ZSTD_LEVEL, PRESIGN_TTL};

/// Ceiling for one bulk transfer (NAR pull, presigned HTTP download, or
/// eval-cache blob) - all three ride the same channel and share one budget.
pub const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Server-side budget for answering one `CacheQuery`; on expiry the server
/// replies `CacheError` so the worker retries instead of reading "uncached".
pub const CACHE_QUERY_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

/// Worker-side wait for `CacheStatus`/`CacheError` and `KnownDerivations`.
pub const CACHE_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// Upper bound on the store paths one `CacheQuery` / `QueryKnownDerivations`
/// may carry. A whole eval's path set (tens of thousands) serialises into a
/// multi-MB request whose reply is larger still; with both peers holding a
/// write the socket can't absorb, neither gets back to reading and the
/// connection deadlocks until a send timeout tears it down. Chunking keeps
/// every request and its reply inside
/// [`crate::handler::SAFE_INFLIGHT_MESSAGE_SIZE`].
pub const CACHE_QUERY_MAX_PATHS: usize = 1_000;

/// How many `CacheQuery` / `QueryKnownDerivations` chunks a worker keeps in
/// flight. Each stays under [`crate::handler::SAFE_INFLIGHT_MESSAGE_SIZE`], so
/// the worst case in flight is that bound times this constant.
pub const CACHE_QUERY_WINDOW: usize = 4;

// The server must give up (and reply CacheError) before the worker stops
// listening, otherwise a slow query reads as a silent miss.
const _: () = assert!(CACHE_QUERY_BUDGET.as_secs() < CACHE_QUERY_TIMEOUT.as_secs());
