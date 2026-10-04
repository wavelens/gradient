/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::Bytes;

use crate::codec::Proto;
use crate::types::{
    BuildFailureKind, BuildMetrics, BuildProgressPhase, CandidateScore, ClusterAddress,
    EvalMessageLevel, EvalProgress, GradientCapabilities, JobKind, JobPhaseSpan, JobUpdateKind,
    QueryMode, UploadMetadata, UploadObject,
};

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
pub enum ClientMessage {
    InitConnection {
        capabilities: GradientCapabilities,
        id: String,
    },

    AuthResponse {
        tokens: Vec<(String, String)>,
    },

    ReauthRequest,

    Reject {
        code: u16,
        reason: String,
    },

    WorkerCapabilities {
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
        cpu_count: u32,
        ram_total_mb: u64,
        cpu_core_score: u32,
        zone: Option<String>,
        endpoint: Option<String>,
    },

    #[proto(removed(28, Option<f32>))]
    WorkerMetrics {
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        #[proto(28, default)]
        upload_speed_mbps: Option<f32>,
        #[proto(28, default)]
        download_speed_mbps: Option<f32>,
    },

    RequestJobList,

    RequestJobChunk {
        scores: Vec<CandidateScore>,
        is_final: bool,
    },

    AssignJobResponse {
        job_id: String,
        accepted: bool,
        reason: Option<String>,
    },

    JobUpdate {
        job_id: String,
        assignment_id: String,
        update: JobUpdateKind,
    },

    JobCompleted {
        job_id: String,
        assignment_id: String,
        spans: Vec<JobPhaseSpan>,
        elapsed_ms: u64,
    },

    JobFailed {
        job_id: String,
        assignment_id: String,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
        spans: Vec<JobPhaseSpan>,
        elapsed_ms: u64,
        #[proto(28, default)]
        metrics: Option<BuildMetrics>,
    },

    Draining,

    BuildProgress {
        job_id: String,
        assignment_id: String,
        build_id: String,
        phase: BuildProgressPhase,
        bytes_done: u64,
        bytes_total: Option<u64>,
        paths_done: u32,
        paths_total: Option<u32>,
    },

    EvalProgress {
        job_id: String,
        assignment_id: String,
        progress: EvalProgress,
    },

    LogChunk {
        job_id: String,
        task_index: u32,
        data: Bytes,
    },

    NarRequest {
        job_id: String,
        paths: Vec<String>,
    },

    NarRequestResume {
        job_id: String,
        store_path: String,
        received_bytes: u64,
        stream_token: String,
    },

    EvalCachePull {
        job_id: String,
        fingerprint: String,
    },

    RequestJob {
        kind: JobKind,
    },

    ClusterSignal {
        attempt: String,
        to: Option<ClusterAddress>,
        payload: Bytes,
    },

    CacheQuery {
        job_id: String,
        /// The server is echoing this id in its `CacheStatus` or `CacheError` reply. Concurrent or
        /// retried queries under one `job_id` can never steal each other's answer.
        query_id: String,
        paths: Vec<String>,
        mode: QueryMode,
        nar_sizes: Vec<Option<u64>>,
        /// Only an external query can consult the upstream caches, and only for the one path named.
        /// A build's inputs are in our cache or the build is failing with `InputsUnavailable`.
        /// Putting them there is a Substitute's job.
        external: bool,
    },

    EvalMessage {
        job_id: String,
        level: EvalMessageLevel,
        source: String,
        message: String,
    },

    QueryKnownDerivations {
        job_id: String,
        query_id: String,
        drv_paths: Vec<String>,
    },

    UploadRequest {
        job_id: String,
        request_id: u64,
        object: UploadObject,
        size: u64,
    },
    UploadChunk {
        request_id: u64,
        data: Bytes,
        offset: u64,
        is_final: bool,
    },
    UploadFinished {
        request_id: u64,
        metadata: UploadMetadata,
    },
    UploadCancel {
        request_id: u64,
    },
}

impl ClientMessage {
    pub fn job_id(&self) -> Option<&str> {
        match self {
            ClientMessage::AssignJobResponse { job_id, .. }
            | ClientMessage::JobUpdate { job_id, .. }
            | ClientMessage::JobCompleted { job_id, .. }
            | ClientMessage::JobFailed { job_id, .. }
            | ClientMessage::BuildProgress { job_id, .. }
            | ClientMessage::EvalProgress { job_id, .. }
            | ClientMessage::LogChunk { job_id, .. }
            | ClientMessage::NarRequest { job_id, .. }
            | ClientMessage::NarRequestResume { job_id, .. }
            | ClientMessage::EvalCachePull { job_id, .. }
            | ClientMessage::CacheQuery { job_id, .. }
            | ClientMessage::EvalMessage { job_id, .. }
            | ClientMessage::QueryKnownDerivations { job_id, .. }
            | ClientMessage::UploadRequest { job_id, .. } => Some(job_id),
            _ => None,
        }
    }

    pub fn variant_name(&self) -> &'static str {
        match self {
            ClientMessage::InitConnection { .. } => "InitConnection",
            ClientMessage::AuthResponse { .. } => "AuthResponse",
            ClientMessage::ReauthRequest => "ReauthRequest",
            ClientMessage::Reject { .. } => "Reject",
            ClientMessage::WorkerCapabilities { .. } => "WorkerCapabilities",
            ClientMessage::WorkerMetrics { .. } => "WorkerMetrics",
            ClientMessage::RequestJobList => "RequestJobList",
            ClientMessage::RequestJobChunk { .. } => "RequestJobChunk",
            ClientMessage::AssignJobResponse { .. } => "AssignJobResponse",
            ClientMessage::JobUpdate { .. } => "JobUpdate",
            ClientMessage::JobCompleted { .. } => "JobCompleted",
            ClientMessage::JobFailed { .. } => "JobFailed",
            ClientMessage::Draining => "Draining",
            ClientMessage::BuildProgress { .. } => "BuildProgress",
            ClientMessage::EvalProgress { .. } => "EvalProgress",
            ClientMessage::LogChunk { .. } => "LogChunk",
            ClientMessage::NarRequest { .. } => "NarRequest",
            ClientMessage::NarRequestResume { .. } => "NarRequestResume",
            ClientMessage::EvalCachePull { .. } => "EvalCachePull",
            ClientMessage::RequestJob { .. } => "RequestJob",
            ClientMessage::ClusterSignal { .. } => "ClusterSignal",
            ClientMessage::CacheQuery { .. } => "CacheQuery",
            ClientMessage::EvalMessage { .. } => "EvalMessage",
            ClientMessage::QueryKnownDerivations { .. } => "QueryKnownDerivations",
            ClientMessage::UploadRequest { .. } => "UploadRequest",
            ClientMessage::UploadChunk { .. } => "UploadChunk",
            ClientMessage::UploadFinished { .. } => "UploadFinished",
            ClientMessage::UploadCancel { .. } => "UploadCancel",
        }
    }

    pub fn carries_secret(&self) -> bool {
        matches!(self, ClientMessage::AuthResponse { .. })
    }
}

#[cfg(test)]
mod job_id_tests {
    use super::*;

    #[test]
    fn job_scoped_messages_name_their_job() {
        let m = ClientMessage::NarRequest {
            job_id: "j1".into(),
            paths: vec![],
        };
        assert_eq!(m.job_id(), Some("j1"));
        assert_eq!(ClientMessage::RequestJobList.job_id(), None);

        let progress = ClientMessage::EvalProgress {
            job_id: "j".into(),
            assignment_id: "a".into(),
            progress: EvalProgress::Evaluating { thunks: 3 },
        };
        assert_eq!(progress.job_id(), Some("j"));
        assert_eq!(progress.variant_name(), "EvalProgress");

        let upload = ClientMessage::BuildProgress {
            job_id: "j".into(),
            assignment_id: "a".into(),
            build_id: "b".into(),
            phase: BuildProgressPhase::Upload,
            bytes_done: 1,
            bytes_total: Some(2),
            paths_done: 0,
            paths_total: Some(1),
        };
        assert_eq!(upload.job_id(), Some("j"));
        assert_eq!(upload.variant_name(), "BuildProgress");
    }
}
