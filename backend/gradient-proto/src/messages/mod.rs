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
    BuildFailureKind, BuildJob, BuildMetrics, BuildOutput, BuildProduct, BuildTask, CacheInfo,
    CachedPath, CandidateScore, CredentialKind, DerivationOutput, DiscoveredDerivation,
    EvalAttrCost, EvalCachePullOutcome, EvalCachePushMode, EvalMessageLevel, EvalStatsReport,
    FlakeInputOverride, FlakeJob, FlakeOutputNode, FlakeSource, FlakeTask, GradientCapabilities,
    Job, JobCandidate, JobKind, JobUpdateKind, QueryMode, RequiredPath,
};
pub use server::{FailedPeer, ServerMessage};
pub use wire::{decode_client_message, decode_server_message};

/// Wire protocol version implemented by this build.
pub const PROTO_VERSION: u16 = 3;
