/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::messages::{
    ClientMessage, FlakeJob, FlakeTask, GradientCapabilities, Job, JobCandidate, PROTO_VERSION,
    ServerMessage,
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
        candidates: vec![
            JobCandidate {
                job_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                required_paths: vec!["/nix/store/abc-foo".into()],
            },
        ],
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
            repository: "https://github.com/example/repo".into(),
            commit: "abc123".into(),
            wildcards: vec!["packages.*".into()],
            timeout_secs: Some(300),
        }),
        timeout_secs: Some(600),
    };
    let bytes = rkyv::to_bytes::<RkyvError>(&original).unwrap();
    let decoded = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes).unwrap();
    assert_eq!(decoded, original);
}

// ── Sanity checks ─────────────────────────────────────────────────────────────

#[test]
fn proto_version_is_nonzero() {
    assert!(PROTO_VERSION >= 1);
}

// Full WebSocket handshake integration tests (InitConnection → InitAck) live in
// the `web` crate's integration tests where `test-support` is available.
