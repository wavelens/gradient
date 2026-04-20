/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::messages::{
    CachedPath, ClientMessage, FlakeJob, FlakeSource, FlakeTask, GradientCapabilities, Job,
    JobCandidate, PROTO_VERSION, QueryMode, RequiredPath, ServerMessage,
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
        }),
        timeout_secs: Some(600),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn cache_query_normal_roundtrip() {
    let original = ClientMessage::CacheQuery {
        job_id: "job-1".into(),
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
        job_id: "job-4".into(),
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
fn cached_path_not_cached_no_url() {
    // Represents an uncached path in Push mode with local (non-S3) storage.
    let cp = CachedPath {
        path: "/nix/store/aaaa-hello".into(),
        cached: false,
        file_size: None,
        nar_size: None,
        url: None,
        nar_hash: None,
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

// ── Sanity checks ─────────────────────────────────────────────────────────────

#[test]
fn proto_version_is_nonzero() {
    let version = PROTO_VERSION;
    assert!(version >= 1);
}

// Full WebSocket handshake integration tests (InitConnection → InitAck) live in
// the `web` crate's integration tests where `test-support` is available.
