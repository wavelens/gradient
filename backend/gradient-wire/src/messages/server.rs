/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::{
    CachedPath, ClusterAddress, ClusterMembership, ClusterPeer, CredentialKind,
    EvalCachePullOutcome, GradientCapabilities, GrantTarget, Job, JobCandidate, UploadOutcome,
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
    /// registered this worker ID - the worker must respond with tokens for
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

    /// Assign a job to this worker.  Worker must respond with
    /// [`super::client::ClientMessage::AssignJobResponse`] before starting
    /// work.
    /// `assignment_id` is the `dispatched_job` id; every report for this job echoes it.
    /// `cluster` is set when the job is one member of a cluster attempt.
    AssignJob {
        job_id: String,
        assignment_id: String,
        job: Job,
        cluster: Option<Box<ClusterMembership>>,
    },

    /// Cancel an in-progress job.  Worker stops, cleans up, and responds
    /// with [`super::client::ClientMessage::JobFailed`].
    AbortJob { job_id: String, reason: String },

    /// Every member of the attempt accepted: run the held jobs.
    StartCluster {
        attempt: String,
        roster: Vec<ClusterPeer>,
    },

    /// A control message another member of the attempt sent to this one.
    ClusterSignal {
        attempt: String,
        from: ClusterAddress,
        payload: Vec<u8>,
    },

    /// Drop or abort every job of the attempt; its slots are free again.
    AbortCluster { attempt: String, reason: String },

    /// Deliver a short-lived credential.  Sent before or alongside
    /// [`ServerMessage::AssignJob`] for steps that need it.
    Credential { kind: CredentialKind, data: Vec<u8> },

    /// One chunk of a NAR being pushed from server to worker (direct mode).
    NarPush {
        job_id: String,
        store_path: String,
        /// zstd-compressed NAR data, 512 KiB chunks (`BULK_CHUNK_SIZE`).
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    },

    /// Sent in response to a [`super::client::ClientMessage::NarRequest`] when
    /// the server cannot serve the requested path at all (e.g. the
    /// `cached_path` row exists but the NAR bytes are not in `nar_storage`).
    /// No `NarPush` chunks will follow for this path. The worker must
    /// resolve any waiter for `(job_id, store_path)` with this `reason`
    /// instead of waiting for `is_final`.
    NarUnavailable {
        job_id: String,
        store_path: String,
        reason: String,
    },

    /// Sent during an in-flight NAR transfer when the server can no longer
    /// continue (e.g. the WebSocket write failed after some chunks, or the
    /// underlying storage stream errored). The worker must discard any
    /// partial buffer for `(job_id, store_path)` and resolve the waiter
    /// with this `reason`. No further `NarPush` chunks will arrive for
    /// this path on this transfer.
    NarAbort {
        job_id: String,
        store_path: String,
        reason: String,
    },

    /// Opens a pull stream for `store_path`; sent before the first `NarPush`
    /// so the worker can size and validate its `.partial`. The server always
    /// knows the stored object's `total_bytes`.
    NarStreamHeader {
        job_id: String,
        store_path: String,
        total_bytes: u64,
        stream_token: String,
    },

    /// Result of a [`super::client::ClientMessage::EvalCachePull`].  The
    /// `outcome` carries a miss, a presigned GET URL, or an inline-stream
    /// header; inline blobs then arrive as [`ServerMessage::EvalCacheChunk`].
    EvalCachePullResult {
        job_id: String,
        outcome: EvalCachePullOutcome,
    },

    /// One chunk of an eval-cache blob being streamed inline to the worker
    /// (local-FS fallback for [`EvalCachePullOutcome::Inline`]).
    EvalCacheChunk {
        job_id: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    },

    /// Response to [`super::client::ClientMessage::CacheQuery`].
    /// In `Pull` mode a local hit carries a presigned GET URL, or `url: None` when
    /// the NAR is pulled over the stream; a path found in an upstream Nix cache
    /// carries its absolute NAR URL. `Normal` and `Push` answers carry no URLs.
    CacheStatus {
        /// Echoes the [`super::client::ClientMessage::CacheQuery`] `query_id`; it is
        /// the sole correlator, so the worker routes this reply to the exact query
        /// that sent it (the worker holds the owning `job_id` locally).
        query_id: String,
        cached: Vec<CachedPath>,
    },

    /// Response to [`super::client::ClientMessage::QueryKnownDerivations`].
    ///
    /// Contains the subset of the queried `.drv` paths whose subtree is already
    /// recorded in the server's derivation table, across every project.
    /// The worker skips subtree traversal for these paths during BFS.
    KnownDerivations {
        /// Echoes the query's `query_id`; the sole correlator, so a worker can
        /// keep several `QueryKnownDerivations` in flight under one `job_id`.
        query_id: String,
        known: Vec<String>,
    },

    /// The server could not *determine* cache state for a
    /// [`super::client::ClientMessage::CacheQuery`] (a transient DB error or an
    /// over-budget handler). Distinct from a `CacheStatus` listing paths as
    /// uncached: the worker must treat this as a retryable transport failure,
    /// never as "inputs missing", so a server-side hiccup cannot poison a build.
    CacheError {
        /// Echoes the [`super::client::ClientMessage::CacheQuery`] `query_id`.
        query_id: String,
        message: String,
    },

    UploadGrant {
        request_id: u64,
        target: GrantTarget,
    },
    UploadCommitted {
        request_id: u64,
        outcome: UploadOutcome,
    },
}

impl ServerMessage {
    pub fn job_id(&self) -> Option<&str> {
        match self {
            ServerMessage::AssignJob { job_id, .. }
            | ServerMessage::AbortJob { job_id, .. }
            | ServerMessage::NarPush { job_id, .. }
            | ServerMessage::NarUnavailable { job_id, .. }
            | ServerMessage::NarAbort { job_id, .. }
            | ServerMessage::NarStreamHeader { job_id, .. }
            | ServerMessage::EvalCachePullResult { job_id, .. }
            | ServerMessage::EvalCacheChunk { job_id, .. } => Some(job_id),
            _ => None,
        }
    }

    /// Static name of the variant. Used for log messages where dumping the
    /// full Debug-formatted message would be unsafe (e.g. `NarPush` carries
    /// megabytes of binary chunk data).
    pub fn variant_name(&self) -> &'static str {
        match self {
            ServerMessage::AuthChallenge { .. } => "AuthChallenge",
            ServerMessage::InitAck { .. } => "InitAck",
            ServerMessage::AuthUpdate { .. } => "AuthUpdate",
            ServerMessage::Reject { .. } => "Reject",
            ServerMessage::Error { .. } => "Error",
            ServerMessage::Draining => "Draining",
            ServerMessage::JobListChunk { .. } => "JobListChunk",
            ServerMessage::JobOffer { .. } => "JobOffer",
            ServerMessage::AssignJob { .. } => "AssignJob",
            ServerMessage::AbortJob { .. } => "AbortJob",
            ServerMessage::StartCluster { .. } => "StartCluster",
            ServerMessage::ClusterSignal { .. } => "ClusterSignal",
            ServerMessage::AbortCluster { .. } => "AbortCluster",
            ServerMessage::Credential { .. } => "Credential",
            ServerMessage::NarPush { .. } => "NarPush",
            ServerMessage::NarUnavailable { .. } => "NarUnavailable",
            ServerMessage::NarAbort { .. } => "NarAbort",
            ServerMessage::NarStreamHeader { .. } => "NarStreamHeader",
            ServerMessage::EvalCachePullResult { .. } => "EvalCachePullResult",
            ServerMessage::EvalCacheChunk { .. } => "EvalCacheChunk",
            ServerMessage::CacheStatus { .. } => "CacheStatus",
            ServerMessage::KnownDerivations { .. } => "KnownDerivations",
            ServerMessage::CacheError { .. } => "CacheError",
            ServerMessage::UploadGrant { .. } => "UploadGrant",
            ServerMessage::UploadCommitted { .. } => "UploadCommitted",
        }
    }
}
