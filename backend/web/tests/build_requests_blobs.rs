/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/{session}/blobs`
//! (issue #234, task 10). Covers the validation surfaces (claimed-hash
//! mismatch, foreign hash, already-dispatched, expired) and the happy
//! path where one blob lands in storage and shrinks `session.missing`.

use axum::http::StatusCode;
use axum_test::multipart::{MultipartForm, Part};
use chrono::{Duration, Utc};
use entity::ids::*;
use entity::role;
use gradient_core::permissions::PermissionMask;
use gradient_core::types::SessionId;
use gradient_core::types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use test_support::fixtures::{org_id, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

fn write_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "write".into(),
        organization: None,
        permission: gradient_core::permissions::write_mask() as PermissionMask,
        managed: false,
    }
}

fn membership() -> entity::organization_user::Model {
    entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: BASE_ROLE_WRITE_ID,
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn live_upload_session(
    id: UploadSessionId,
    missing: Vec<String>,
    dispatched: bool,
    expired: bool,
) -> entity::upload_session::Model {
    let now = Utc::now().naive_utc();
    let expires_at = if expired {
        now - Duration::seconds(60)
    } else {
        now + Duration::hours(1)
    };
    entity::upload_session::Model {
        id,
        organization: org_id(),
        manifest: json!([]),
        missing: serde_json::to_value(missing).unwrap(),
        total_size: 0,
        created_at: now,
        expires_at,
        dispatched_at: if dispatched { Some(now) } else { None },
    }
}

fn hex_hash_of(bytes: &[u8]) -> String {
    hex::encode(blake3::hash(bytes).as_bytes())
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn blobs_url(session: UploadSessionId) -> String {
    format!("/api/v1/build-requests/{}/blobs", session)
}

#[test]
fn rejects_hash_mismatch() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let payload = b"actual file contents".to_vec();
        let claimed_hex = hex_hash_of(b"different bytes");

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![live_upload_session(
                upload,
                vec![claimed_hex.clone()],
                false,
                false,
            )]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(claimed_hex, Part::bytes(payload));

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
    });
}

#[test]
fn rejects_foreign_hash() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let payload = b"hello".to_vec();
        let payload_hex = hex_hash_of(&payload);
        let other_hex = hex_hash_of(b"other content");

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![live_upload_session(
                upload,
                vec![other_hex],
                false,
                false,
            )]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
    });
}

#[test]
fn happy_path_uploads_blob_and_shrinks_missing() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let payload = b"a real source file".to_vec();
        let payload_hex = hex_hash_of(&payload);

        let session_row = live_upload_session(upload, vec![payload_hex.clone()], false, false);

        let inserted_blob = entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            organization: org_id(),
            hash: hex::decode(&payload_hex).unwrap(),
            size: payload.len() as i64,
            created_at: Utc::now().naive_utc(),
            last_used_at: Utc::now().naive_utc(),
        };
        let updated_session = entity::upload_session::Model {
            missing: json!([]),
            ..session_row.clone()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![session_row]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]])
            .append_query_results([vec![inserted_blob]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![updated_session]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["uploaded"], 1);
        assert_eq!(body["message"]["remaining"], 0);
    });
}

#[test]
fn rejects_already_dispatched_session() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let payload = b"hello".to_vec();
        let payload_hex = hex_hash_of(&payload);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![live_upload_session(
                upload,
                vec![payload_hex.clone()],
                true,
                false,
            )]]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::CONFLICT);
    });
}

#[test]
fn rejects_expired_session() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let payload = b"hello".to_vec();
        let payload_hex = hex_hash_of(&payload);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![live_upload_session(
                upload,
                vec![payload_hex.clone()],
                false,
                true,
            )]]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::GONE);
    });
}

#[test]
fn rejects_session_not_found() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([Vec::<entity::upload_session::Model>::new()]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_part(
            "a".repeat(64),
            Part::bytes(b"whatever".to_vec()),
        );

        let res = server
            .post(&blobs_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}

