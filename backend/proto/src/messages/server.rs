/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::proto::GradientCapabilities;
use rkyv::{Archive, Deserialize, Serialize};

use super::jobs::Job;
use super::types::{CredentialKind, JobCandidate};

/// Messages sent from the server to the client (worker / federated peer).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum ServerMessage {
    /// Successful handshake response.  Contains the negotiated capabilities.
    InitAck {
        version: u16,
        capabilities: GradientCapabilities,
    },

    /// Server declines the connection.  Closes after sending.
    Reject { code: u16, reason: String },

    /// Protocol-level error.  The connection may be closed after this.
    Error { code: u16, message: String },

    /// Server is shutting down gracefully.  Workers should finish in-flight
    /// jobs, buffer results, and delay reconnection.
    Draining,

    /// Chunk of the full job candidate list, sent in response to
    /// [`super::client::ClientMessage::RequestJobList`].
    /// `is_final: true` marks the end.
    JobListChunk {
        candidates: Vec<JobCandidate>,
        is_final: bool,
    },

    /// Incremental push of new job candidates as they become available
    /// (e.g. evaluation discovers new derivations).
    JobOffer { candidates: Vec<JobCandidate> },

    /// Remove candidates from the worker's local cache — they have been
    /// assigned to another worker or cancelled.
    RevokeJob { job_ids: Vec<String> },

    /// Assign a job to this worker.  Worker must respond with
    /// [`super::client::ClientMessage::AssignJobResponse`] before starting
    /// work.
    AssignJob {
        job_id: String,
        job: Job,
        /// Wall-clock limit in seconds.  `None` = no timeout.
        timeout_secs: Option<u64>,
    },

    /// Cancel an in-progress job.  Worker stops, cleans up, and responds
    /// with [`super::client::ClientMessage::JobFailed`].
    AbortJob { job_id: String, reason: String },

    /// Deliver a short-lived credential.  Sent before or alongside
    /// [`ServerMessage::AssignJob`] for tasks that need it.
    Credential { kind: CredentialKind, data: Vec<u8> },

    /// One chunk of a NAR being pushed from server to worker (direct mode).
    NarPush {
        job_id: String,
        store_path: String,
        /// zstd-compressed NAR data, 64 KiB chunks.
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    },

    /// Presigned S3 upload URL for a build output.  Worker uploads directly
    /// then confirms with [`super::client::ClientMessage::NarReady`].
    PresignedUpload {
        job_id: String,
        store_path: String,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
    },

    /// Presigned S3 download URL for a required store path.
    PresignedDownload {
        job_id: String,
        store_path: String,
        url: String,
    },
}
