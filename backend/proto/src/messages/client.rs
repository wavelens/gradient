/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::proto::{CandidateScore, GradientCapabilities, JobUpdateKind};
use rkyv::{Archive, Deserialize, Serialize};

/// Messages sent from the client (worker / federated peer) to the server.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum ClientMessage {
    /// First message on every connection.  The peer declares its protocol
    /// version, capabilities, and persistent identity.  The server responds
    /// with [`super::server::ServerMessage::AuthChallenge`].
    InitConnection {
        version: u16,
        capabilities: GradientCapabilities,
        /// Persistent peer UUID, generated on first start and stored locally.
        id: String,
    },

    /// Response to [`super::server::ServerMessage::AuthChallenge`].
    /// Contains per-peer tokens for each peer the worker has credentials for.
    /// Pairs are `(peer_id, token)`.
    AuthResponse { tokens: Vec<(String, String)> },

    /// Request a new auth challenge from the server — sent when the worker
    /// has acquired a new peer token and wants to become authorized for that
    /// peer without reconnecting.
    ReauthRequest,

    /// Decline the connection after receiving
    /// [`super::server::ServerMessage::InitAck`].
    /// The peer closes the WebSocket immediately after sending this.
    Reject { code: u16, reason: String },

    /// Advertise build capacity.  Sent after a successful handshake by any
    /// peer with the `build` capability negotiated.
    WorkerCapabilities {
        /// Supported architectures as Nix system strings, e.g. `"x86_64-linux"`.
        architectures: Vec<String>,
        /// Nix system features (e.g. `"kvm"`, `"big-parallel"`), capacity-sorted.
        system_features: Vec<String>,
        /// Maximum number of concurrent builds this peer accepts.
        max_concurrent_builds: u32,
    },

    /// Request the full current job candidate list as a stream of
    /// [`super::server::ServerMessage::JobListChunk`] messages.  Sent once
    /// after the handshake to bootstrap the local candidate cache.
    RequestJobList,

    /// Stream pre-computed job scores to the server.  Sent incrementally as
    /// the worker checks `required_paths` against its local Nix store.
    /// `is_final: true` marks the last chunk for the current scoring pass.
    RequestJobChunk {
        scores: Vec<CandidateScore>,
        is_final: bool,
    },

    /// Accept or reject a [`super::server::ServerMessage::AssignJob`].
    AssignJobResponse {
        job_id: String,
        accepted: bool,
        /// Set when `accepted` is `false`.
        reason: Option<String>,
    },

    /// Incremental progress update for an in-flight job.
    /// The server maps these directly to `EvaluationStatus` / `BuildStatus`.
    JobUpdate {
        job_id: String,
        update: JobUpdateKind,
    },

    /// All tasks in a job completed successfully.
    /// Results were already sent via [`ClientMessage::JobUpdate`].
    JobCompleted { job_id: String },

    /// A task in the job failed; remaining tasks are skipped.
    JobFailed { job_id: String, error: String },

    /// Worker is draining — it will finish in-flight jobs then disconnect.
    /// Server stops assigning new jobs to this peer.
    Draining,

    /// Build log lines from an in-flight task.  Fire-and-forget.
    LogChunk {
        job_id: String,
        task_index: u32,
        data: Vec<u8>,
    },

    /// Request specific store paths from the server (direct NAR mode).
    NarRequest { job_id: String, paths: Vec<String> },

    /// One chunk of a NAR being pushed from worker to server (direct mode).
    NarPush {
        job_id: String,
        store_path: String,
        /// zstd-compressed NAR data, 64 KiB chunks.
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    },

    /// Worker has finished uploading a NAR directly to S3 and reports its
    /// hash/size so the server can record the path info.
    NarReady {
        job_id: String,
        store_path: String,
        nar_size: u64,
        /// SRI-format hash: `sha256-<base64>`.
        nar_hash: String,
    },
}
