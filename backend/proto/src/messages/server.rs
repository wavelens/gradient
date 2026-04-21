/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::proto::{
    CachedPath, CredentialKind, GradientCapabilities, Job, JobCandidate,
};
use rkyv::{Archive, Deserialize, Serialize};

/// A peer that failed authentication during the challenge-response flow.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct FailedPeer {
    pub peer_id: String,
    pub reason: String,
}

/// Messages sent from the server to the client (worker / federated peer).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum ServerMessage {
    /// Challenge sent after `InitConnection`.  Lists the peer IDs that have
    /// registered this worker ID — the worker must respond with tokens for
    /// each peer it has credentials for.
    AuthChallenge { peers: Vec<String> },

    /// Successful handshake response.  Contains the negotiated capabilities
    /// and the set of peers this worker is now authorized for.
    InitAck {
        version: u16,
        capabilities: GradientCapabilities,
        /// Peer IDs whose tokens were accepted.
        authorized_peers: Vec<String>,
        /// Peers whose tokens were missing or invalid.
        failed_peers: Vec<FailedPeer>,
    },

    /// Sent after a mid-connection reauth completes (triggered by
    /// [`super::client::ClientMessage::ReauthRequest`] or by the server when
    /// a new peer registers this worker).
    AuthUpdate {
        authorized_peers: Vec<String>,
        failed_peers: Vec<FailedPeer>,
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
    /// Paginated at 1 000 entries per message.
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
    /// then confirms with [`super::client::ClientMessage::NarUploaded`].
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

    /// Ask a newly connected worker to send its full candidate score set.
    /// Sent once by the server during the initial handshake completion so it
    /// can populate its in-memory score table.  After startup all score
    /// updates arrive as delta [`super::client::ClientMessage::RequestJobChunk`]
    /// messages — `RequestAllScores` is not sent again.
    RequestAllScores,

    /// Response to [`super::client::ClientMessage::CacheQuery`].
    /// Paths in the local Gradient cache have `url: None`; paths found in upstream
    /// external Nix caches have `url: Some(absolute_nar_url)`.
    CacheStatus {
        job_id: String,
        cached: Vec<CachedPath>,
    },

    /// Response to [`super::client::ClientMessage::QueryKnownDerivations`].
    ///
    /// Contains the subset of the queried `.drv` paths that are already
    /// recorded in the server's derivation table for the owning org.
    /// The worker skips subtree traversal for these paths during BFS.
    KnownDerivations {
        job_id: String,
        known: Vec<String>,
    },
}
