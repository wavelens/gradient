/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::Bytes;

use crate::codec::Proto;
use crate::types::{
    CachedPath, ClusterAddress, ClusterMembership, ClusterPeer, CredentialKind,
    EvalCachePullOutcome, GradientCapabilities, GrantTarget, Job, JobCandidate, UploadOutcome,
};

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct FailedPeer {
    pub peer_id: String,
    pub reason: String,
}

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
pub enum ServerMessage {
    AuthChallenge {
        peers: Vec<String>,
    },

    InitAck {
        capabilities: GradientCapabilities,
        authorized_peers: Vec<String>,
        failed_peers: Vec<FailedPeer>,
    },

    AuthUpdate {
        authorized_peers: Vec<String>,
        failed_peers: Vec<FailedPeer>,
    },

    Reject {
        code: u16,
        reason: String,
    },

    Error {
        code: u16,
        message: String,
    },

    Draining,

    JobListChunk {
        candidates: Vec<JobCandidate>,
        is_final: bool,
    },

    JobOffer {
        candidates: Vec<JobCandidate>,
    },

    AssignJob {
        job_id: String,
        assignment_id: String,
        job: Job,
        cluster: Option<Box<ClusterMembership>>,
    },

    AbortJob {
        job_id: String,
        reason: String,
    },

    StartCluster {
        attempt: String,
        roster: Vec<ClusterPeer>,
    },

    ClusterSignal {
        attempt: String,
        from: ClusterAddress,
        payload: Bytes,
    },

    AbortCluster {
        attempt: String,
        reason: String,
    },

    Credential {
        kind: CredentialKind,
        data: Bytes,
    },

    NarPush {
        job_id: String,
        store_path: String,
        data: Bytes,
        offset: u64,
        is_final: bool,
    },

    /// No `NarPush` chunk is following for this path. The worker must resolve any waiter for
    /// `(job_id, store_path)` with this `reason` instead of waiting for `is_final`.
    NarUnavailable {
        job_id: String,
        store_path: String,
        reason: String,
    },

    NarAbort {
        job_id: String,
        store_path: String,
        reason: String,
    },

    NarStreamHeader {
        job_id: String,
        store_path: String,
        total_bytes: u64,
        stream_token: String,
    },

    EvalCachePullResult {
        job_id: String,
        outcome: EvalCachePullOutcome,
    },

    EvalCacheChunk {
        job_id: String,
        data: Bytes,
        offset: u64,
        is_final: bool,
    },

    CacheStatus {
        query_id: String,
        cached: Vec<CachedPath>,
    },

    KnownDerivations {
        query_id: String,
        known: Vec<String>,
    },

    /// The worker must treat this as a retryable transport failure, never as missing inputs. A
    /// server-side hiccup can then never poison a build.
    CacheError {
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
    Authenticate {
        worker_id: String,
        tokens: Vec<(String, String)>,
    },
    #[proto(29)]
    Handover {
        index: u32,
        paths: Vec<String>,
        is_final: bool,
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
            ServerMessage::Authenticate { .. } => "Authenticate",
            ServerMessage::Handover { .. } => "Handover",
        }
    }

    pub fn carries_secret(&self) -> bool {
        matches!(
            self,
            ServerMessage::Authenticate { .. } | ServerMessage::Credential { .. }
        )
    }
}
