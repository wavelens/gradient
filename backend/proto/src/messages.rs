/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_core::types::proto::GradientCapabilities;
use rkyv::{Archive, Deserialize, Serialize};

/// Wire protocol version implemented by this build.
pub const PROTO_VERSION: u16 = 1;

/// Messages sent from the client (builder agent) to the server.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum ClientMessage {
    /// First message on every new connection.  The client declares its
    /// protocol version and the capabilities it supports.
    /// The server responds with [`ServerMessage::InitAck`].
    InitConnection {
        version: u16,
        capabilities: GradientCapabilities,
        /// Persistent peer identity (worker or server), generated on first start.
        id: String,
        /// API key for authentication. Not required for cache-only connections.
        token: Option<String>,
    },
    /// Request the full list of available job candidates.
    /// Sent after the handshake so the worker can score and pick work.
    /// The server responds with [`ServerMessage::JobListChunk`].
    RequestJobList,
    /// Response to [`ServerMessage::AssignJob`].  The worker either accepts
    /// the job (work begins) or rejects it with a reason (server reassigns).
    AssignJobResponse {
        job_id: String,
        accepted: bool,
        /// Reason for rejection (only set when `accepted` is false).
        reason: Option<String>,
    },
}

/// Messages sent from the server to the client (builder agent).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum ServerMessage {
    /// Sent in response to [`ClientMessage::InitConnection`].
    /// Contains the negotiated protocol version and the capabilities
    /// the server is willing to activate for this session.
    InitAck {
        version: u16,
        capabilities: GradientCapabilities,
    },
    /// A chunk of job candidates, sent in response to
    /// [`ClientMessage::RequestJobList`].  Large lists are streamed across
    /// multiple `JobListChunk` messages; `is_final` marks the last one.
    /// After the initial snapshot the server pushes incremental updates.
    JobListChunk {
        candidates: Vec<JobCandidate>,
        is_final: bool,
    },
    /// Assign a job to this worker.  The worker must respond with
    /// [`ClientMessage::AssignJobResponse`] before starting work.
    AssignJob {
        job_id: String,
    },
    /// Server is shutting down gracefully.  Workers should finish in-flight
    /// jobs, buffer results, and not reconnect until after a delay.
    Draining,
    /// A protocol-level error.  The connection may be closed after this.
    Error { code: u16, message: String },
}

/// A job candidate advertised by the server.  Workers use `required_paths`
/// to compute a missing-path score and request the best-fit job.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct JobCandidate {
    pub job_id: String,
    /// Store paths the job needs — workers check their local store to
    /// compute a substitution score.
    pub required_paths: Vec<String>,
}
