/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::messages::{
    BuildMetrics, BuildOutput, CachedPath, ClientMessage, EvalCachePullOutcome, EvalCachePushMode,
    FlakeInputOverride, FlakeJob, FlakeSource, FlakeTask, GradientCapabilities, Job, JobCandidate,
    JobUpdateKind, PROTO_VERSION, QueryMode, RequiredPath, ServerMessage,
};
use rkyv::rancor::Error as RkyvError;

// ── Message round-trip (rkyv serialize → deserialize) ────────────────────────

#[test]
fn init_connection_roundtrip() {
    let original = ClientMessage::InitConnection {
        version: PROTO_VERSION,
        capabilities: GradientCapabilities::default(),
        id: "550e8400-e29b-41d4-a716-446655440000".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn init_ack_roundtrip() {
    let original = ServerMessage::InitAck {
        version: PROTO_VERSION,
        capabilities: GradientCapabilities::default(),
        authorized_peers: vec!["peer-1".into()],
        failed_peers: vec![],
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn error_message_roundtrip() {
    let original = ServerMessage::Error {
        code: 400,
        message: "unsupported protocol version 99".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn request_job_list_roundtrip() {
    let original = ClientMessage::RequestJobList;
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn job_list_chunk_roundtrip() {
    let original = ServerMessage::JobListChunk {
        candidates: vec![JobCandidate {
            job_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            required_paths: vec![RequiredPath {
                path: "/nix/store/abc-foo".into(),
                cache_info: None,
            }],
            drv_paths: vec![],
        }],
        is_final: false,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn job_list_chunk_final_roundtrip() {
    let original = ServerMessage::JobListChunk {
        candidates: vec![],
        is_final: true,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn assign_job_response_roundtrip() {
    let original = ClientMessage::AssignJobResponse {
        job_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        accepted: true,
        reason: None,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn assign_job_response_reject_roundtrip() {
    let original = ClientMessage::AssignJobResponse {
        job_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        accepted: false,
        reason: Some("no capacity".into()),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn server_draining_roundtrip() {
    let original = ServerMessage::Draining;
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn assign_job_roundtrip() {
    let original = ServerMessage::AssignJob {
        job_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        job: Job::Flake(FlakeJob {
            tasks: vec![FlakeTask::FetchFlake, FlakeTask::EvaluateFlake],
            source: FlakeSource::Repository {
                url: "https://github.com/example/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["packages.*".into()],
            timeout_secs: Some(300),
            input_overrides: vec![],
            input_update: None,
        }),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn flake_input_override_roundtrip() {
    let job = FlakeJob {
        tasks: vec![FlakeTask::FetchFlake, FlakeTask::EvaluateFlake],
        source: FlakeSource::Repository {
            url: "https://example.test/repo.git".into(),
            commit: "deadbeef".into(),
        },
        wildcards: vec!["packages.x86_64-linux.*".into()],
        timeout_secs: None,
        input_overrides: vec![
            FlakeInputOverride {
                input_name: "nixpkgs".into(),
                url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
            },
            FlakeInputOverride {
                input_name: "flake-utils".into(),
                url: None,
            },
        ],
        input_update: None,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&job).unwrap();
    let decoded: FlakeJob = rkyv::from_bytes::<_, RkyvError>(&bytes[..]).unwrap();
    assert_eq!(decoded, job);
}

#[test]
fn cache_query_normal_roundtrip() {
    let original = ClientMessage::CacheQuery {
        job_id: "job-1".into(),
        query_id: "query-1".into(),
        paths: vec!["/nix/store/aaaa-hello".into()],
        mode: QueryMode::Normal,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cache_query_push_roundtrip() {
    let original = ClientMessage::CacheQuery {
        job_id: "job-2".into(),
        query_id: "query-2".into(),
        paths: vec!["/nix/store/aaaa-foo".into(), "/nix/store/bbbb-bar".into()],
        mode: QueryMode::Push,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cache_query_pull_roundtrip() {
    let original = ClientMessage::CacheQuery {
        job_id: "job-3".into(),
        query_id: "query-3".into(),
        paths: vec!["/nix/store/cccc-baz".into()],
        mode: QueryMode::Pull,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cache_status_roundtrip() {
    let original = ServerMessage::CacheStatus {
        query_id: "query-4a".into(),
        cached: vec![
            CachedPath {
                path: "/nix/store/aaaa-foo".into(),
                cached: true,
                file_size: Some(1024),
                nar_size: Some(4096),
                url: None,
                nar_hash: Some(
                    "sha256:0000000000000000000000000000000000000000000000000000".into(),
                ),
                file_hash: Some(
                    "sha256:1111111111111111111111111111111111111111111111111111".into(),
                ),
                references: Some(vec!["/nix/store/cccc-dep".into()]),
                signatures: Some(vec!["cache.example.com-1:abcd".into()]),
                deriver: Some("/nix/store/aaaa-foo.drv".into()),
                ca: None,
            },
            CachedPath {
                path: "/nix/store/bbbb-bar".into(),
                cached: false,
                file_size: None,
                nar_size: None,
                url: Some("https://s3.example.com/nars/bb/bb.nar.zst".into()),
                nar_hash: None,
                file_hash: None,
                references: None,
                signatures: None,
                deriver: None,
                ca: None,
            },
        ],
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cache_error_roundtrip() {
    let original = ServerMessage::CacheError {
        query_id: "query-5a".into(),
        message: "cache lookup failed: Connection pool timed out".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cached_path_not_cached_no_url() {
    // Represents an uncached path in Push mode with local (non-S3) storage.
    let cp = CachedPath {
        path: "/nix/store/aaaa-hello".into(),
        cached: false,
        file_size: None,
        nar_size: None,
        url: None,
        nar_hash: None,
        file_hash: None,
        references: None,
        signatures: None,
        deriver: None,
        ca: None,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&cp).unwrap();
    let decoded = rkyv::from_bytes::<CachedPath, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, cp);
    assert!(!decoded.cached);
    assert!(decoded.url.is_none());
}

#[test]
fn job_completed_roundtrip() {
    let original = ClientMessage::JobCompleted {
        job_id: "job-123".to_string(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn build_output_with_metrics_roundtrip() {
    let original = ClientMessage::JobUpdate {
        job_id: "job-123".to_string(),
        update: JobUpdateKind::BuildOutput {
            build_id: "build-1".to_string(),
            outputs: vec![BuildOutput {
                name: "out".into(),
                store_path: "/nix/store/aaaa-foo".into(),
                hash: "aaaa".into(),
                nar_size: Some(4096),
                nar_hash: Some("sha256:abc".into()),
                products: vec![],
            }],
            metrics: Some(BuildMetrics {
                peak_ram_mb: Some(2048),
                cpu_time_ms: Some(60_000),
                avg_cpu_pct: Some(50.0),
                disk_read_bytes: Some(1024),
                disk_write_bytes: Some(2048),
                oom_killed: true,
                build_time_ms: Some(120_000),
                peak_network_mbps: Some(42.0),
            }),
            substituted: false,
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn build_output_no_metrics_roundtrip() {
    let original = ClientMessage::JobUpdate {
        job_id: "job-456".to_string(),
        update: JobUpdateKind::BuildOutput {
            build_id: "build-2".to_string(),
            outputs: vec![],
            metrics: None,
            substituted: true,
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

// ── Sanity checks ─────────────────────────────────────────────────────────────

#[test]
fn proto_version_is_nonzero() {
    let version = PROTO_VERSION;
    assert!(version >= 1);
}

/// Regression for #110: the `/proto` WebSocket cap must:
/// - exceed the largest legitimate frame (`NarPush` at 4 MiB plus rkyv
///   overhead, plus headroom for `LogChunk`/`CacheQuery` arrays), and
/// - stay well below tungstenite's 64 MiB default so a malicious peer can't
///   ask the server to allocate gigabytes from a single send.
#[test]
fn max_proto_message_size_is_sane() {
    use crate::handler::{MAX_PROTO_MESSAGE_SIZE, NAR_PUSH_CHUNK_SIZE};
    const _: () = {
        assert!(
            MAX_PROTO_MESSAGE_SIZE >= NAR_PUSH_CHUNK_SIZE * 2,
            "must fit a NarPush chunk plus framing/metadata",
        );
        assert!(
            MAX_PROTO_MESSAGE_SIZE <= 16 * 1024 * 1024,
            "guard against accidental relaxation back toward defaults",
        );
    };
}

/// Regression for #110: the handshake deadline must be long enough to cover
/// a real auth round-trip but short enough that a stalled peer cannot pin a
/// task and FD for minutes.
#[test]
fn handshake_timeout_is_sane() {
    use crate::handler::HANDSHAKE_TIMEOUT;
    assert!(HANDSHAKE_TIMEOUT.as_secs() >= 5);
    assert!(HANDSHAKE_TIMEOUT.as_secs() <= 60);
}

// ── Resumable NAR transfer messages (#225) ───────────────────────────────────

#[test]
fn nar_stream_header_client_roundtrip() {
    let original = ClientMessage::NarStreamHeader {
        job_id: "job-1".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".into(),
        total_bytes: Some(4096),
        stream_token: "zstd6-fmt1-lib10506".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn nar_request_resume_roundtrip() {
    let original = ClientMessage::NarRequestResume {
        job_id: "job-1".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".into(),
        received_bytes: 8_388_608,
        stream_token: "zstd6-fmt1-lib10506".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn nar_stream_header_server_roundtrip() {
    let original = ServerMessage::NarStreamHeader {
        job_id: "job-1".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".into(),
        total_bytes: 4096,
        stream_token: "len-4096".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn nar_push_resume_roundtrip() {
    let original = ServerMessage::NarPushResume {
        job_id: "job-1".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".into(),
        received_bytes: 8_388_608,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

// ── Eval-cache transfer messages (#386 L3) ───────────────────────────────────

#[test]
fn eval_cache_pull_roundtrip() {
    let original = ClientMessage::EvalCachePull {
        job_id: "job-1".into(),
        fingerprint: "blake3:deadbeef".into(),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_push_roundtrip() {
    let original = ClientMessage::EvalCachePush {
        job_id: "job-1".into(),
        fingerprint: "blake3:deadbeef".into(),
        size_bytes: 1_048_576,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_push_done_roundtrip() {
    let original = ClientMessage::EvalCachePushDone {
        job_id: "job-1".into(),
        fingerprint: "blake3:deadbeef".into(),
        size_bytes: 1_048_576,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_chunk_client_roundtrip() {
    let original = ClientMessage::EvalCacheChunk {
        job_id: "job-1".into(),
        data: vec![1, 2, 3, 4],
        offset: 8_388_608,
        is_final: true,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_pull_result_miss_roundtrip() {
    let original = ServerMessage::EvalCachePullResult {
        job_id: "job-1".into(),
        outcome: EvalCachePullOutcome::Miss,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_pull_result_presigned_roundtrip() {
    let original = ServerMessage::EvalCachePullResult {
        job_id: "job-1".into(),
        outcome: EvalCachePullOutcome::Presigned {
            url: "https://s3.example.com/eval-cache/de/deadbeef.sqlite".into(),
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_pull_result_inline_roundtrip() {
    let original = ServerMessage::EvalCachePullResult {
        job_id: "job-1".into(),
        outcome: EvalCachePullOutcome::Inline {
            total_bytes: 1_048_576,
            stream_token: "evalcache-deadbeef".into(),
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_chunk_server_roundtrip() {
    let original = ServerMessage::EvalCacheChunk {
        job_id: "job-1".into(),
        data: vec![9, 8, 7],
        offset: 0,
        is_final: false,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_push_grant_skip_roundtrip() {
    let original = ServerMessage::EvalCachePushGrant {
        job_id: "job-1".into(),
        mode: EvalCachePushMode::Skip,
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_push_grant_presigned_roundtrip() {
    let original = ServerMessage::EvalCachePushGrant {
        job_id: "job-1".into(),
        mode: EvalCachePushMode::Presigned {
            url: "https://s3.example.com/eval-cache/de/deadbeef.sqlite?put".into(),
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn eval_cache_push_grant_inline_roundtrip() {
    let original = ServerMessage::EvalCachePushGrant {
        job_id: "job-1".into(),
        mode: EvalCachePushMode::Inline {
            stream_token: "evalcache-deadbeef".into(),
        },
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

// Full WebSocket handshake integration tests (InitConnection → InitAck) live in
// the `web` crate's integration tests where `test-support` is available.
