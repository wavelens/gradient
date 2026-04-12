/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod client;
pub mod jobs;
pub mod server;
pub mod types;

pub use client::ClientMessage;
pub use gradient_core::types::proto::GradientCapabilities;
pub use jobs::{
    BuildJob, BuildTask, CompressTask, FlakeJob, FlakeTask, Job, JobUpdateKind, SignTask,
};
pub use server::{FailedPeer, ServerMessage};
pub use types::{
    BuildOutput, CandidateScore, CredentialKind, DerivationOutput, DiscoveredDerivation,
    JobCandidate,
};

/// Wire protocol version implemented by this build.
pub const PROTO_VERSION: u16 = 1;
