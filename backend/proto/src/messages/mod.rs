/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod client;
pub mod server;

// Job and scheduling types live in core::types::proto — re-exported here for
// backward compatibility so existing `proto::messages::FlakeJob` paths still work.
pub use client::ClientMessage;
pub use gradient_core::types::proto::{
    BuildJob, BuildOutput, BuildTask, CacheInfo, CachedPath, CandidateScore, CompressTask,
    CredentialKind, DerivationOutput, DiscoveredDerivation, FetchedInput, FlakeJob, FlakeTask,
    GradientCapabilities, Job, JobCandidate, JobKind, JobUpdateKind, QueryMode, RequiredPath,
    SignTask,
};
pub use server::{FailedPeer, ServerMessage};

/// Wire protocol version implemented by this build.
pub const PROTO_VERSION: u16 = 1;
