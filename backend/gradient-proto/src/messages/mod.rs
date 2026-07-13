/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod client;
pub mod server;
pub mod wire;

// Job and scheduling types live in gradient_types::proto - re-exported here for
// backward compatibility so existing `crate::messages::FlakeJob` paths still work.
pub use client::ClientMessage;
pub use gradient_types::proto::{
    BuildFailureKind, BuildJob, BuildMetrics, BuildOutput, BuildProduct, BuildTask,
    BumpedInputWire, CacheInfo, CachedPath, CandidateScore, CredentialKind, DerivationOutput,
    DiscoveredDerivation, EvalAttrCost, EvalCachePullOutcome, EvalCachePushMode, EvalMessageLevel,
    EvalStatsReport, FlakeInputOverride, FlakeJob, FlakeOutputNode, FlakeSource, FlakeTask,
    GradientCapabilities, InputUpdateSpec, Job, JobCandidate, JobKind, JobUpdateKind, QueryMode,
    RequiredPath,
};
pub use server::{FailedPeer, ServerMessage};
pub use wire::{decode_client_message, decode_server_message};

/// Wire protocol version implemented by this build.
/// v5: dropped `PresignedUpload`/`PresignedDownload` and `AssignJob.timeout_secs`.
/// v7: `CacheQuery`/`CacheStatus`/`CacheError` carry a per-query `query_id`;
///     `NarUploaded` carries the path's content address (`ca`).
pub const PROTO_VERSION: u16 = 7;

pub use gradient_types::constants::{NAR_ZSTD_LEVEL, PRESIGN_TTL};

/// Ceiling for one bulk transfer (NAR pull, presigned HTTP download, or
/// eval-cache blob) - all three ride the same channel and share one budget.
pub const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Server-side budget for answering one `CacheQuery`; on expiry the server
/// replies `CacheError` so the worker retries instead of reading "uncached".
pub const CACHE_QUERY_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

/// Worker-side wait for `CacheStatus`/`CacheError` and `KnownDerivations`.
pub const CACHE_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

// The server must give up (and reply CacheError) before the worker stops
// listening, otherwise a slow query reads as a silent miss.
const _: () = assert!(CACHE_QUERY_BUDGET.as_secs() < CACHE_QUERY_TIMEOUT.as_secs());
