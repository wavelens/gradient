/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod client;
pub mod server;

pub use crate::types::{
    BuildFailureKind, BuildJob, BuildMetrics, BuildOutput, BuildProduct, BuildSpec, BuildSpecKind,
    BumpedInputWire, CacheInfo, CachedPath, CandidateScore, ClusterAddress, ClusterMembership,
    ClusterPeer, CredentialKind, DerivationOutput, DiscoveredDerivation, EvalAttrCost,
    EvalCachePullOutcome, EvalMessageLevel, EvalProgress, EvalStatsReport, FlakeInputOverride,
    FlakeJob, FlakeOutputNode, FlakeSource, FlakeStep, GradientCapabilities, InputFetch,
    InputFetchState, InputUpdateSpec, Job, JobCandidate, JobKind, JobPhase, JobPhaseSpan,
    JobUpdateKind, QueryMode, RequiredPath,
};
pub use crate::types::{
    CompletedMultipart, GrantTarget, NarUploadMetadata, PresignedMultipart, UploadMetadata,
    UploadObject, UploadOutcome,
};
pub use client::{ArchivedClientMessage, ClientMessage};
pub use server::{ArchivedServerMessage, FailedPeer, ServerMessage};

pub const PROTO_VERSION: u16 = 25;

pub const BUILD_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

pub const EVAL_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

pub use crate::constants::{NAR_ZSTD_LEVEL, PRESIGN_TTL};

pub const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

pub const SMALL_UPLOAD_BYTES: u64 = 1024 * 1024;

pub fn is_small_upload(size: u64) -> bool {
    size <= SMALL_UPLOAD_BYTES
}

pub const SMALL_UPLOADS_IN_FLIGHT: usize = 128;

pub const CACHE_QUERY_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

pub const CACHE_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// A whole evaluation's path set is serialising into a multi-MB request with a larger reply. Both
/// peers are then holding a write the socket cannot absorb, and the connection is deadlocking until
/// a send timeout. Chunking is keeping every request and its reply inside
/// [`crate::session::frame::SAFE_INFLIGHT_MESSAGE_SIZE`].
pub const CACHE_QUERY_MAX_PATHS: usize = 1_000;

pub const CACHE_QUERY_WINDOW: usize = 4;

// The server must give up and reply `CacheError` while the worker is still listening. A slow query
// would otherwise read as a silent miss.
const _: () = assert!(CACHE_QUERY_BUDGET.as_secs() < CACHE_QUERY_TIMEOUT.as_secs());
