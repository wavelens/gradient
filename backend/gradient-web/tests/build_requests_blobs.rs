/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::multipart::{MultipartForm, Part};
use chrono::Utc;
use gradient_entity::ids::*;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows::{project_member, upload_session, write_role};
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::MockExecResult;
use serde_json::{Value, json};

fn hex_hash_of(bytes: &[u8]) -> String {
    hex::encode(blake3::hash(bytes).as_bytes())
}

fn blobs_url(session: UploadSessionId) -> String {
    format!("/api/v1/build-requests/{}/blobs", session)
}

#[tokio::test]
async fn rejects_hash_mismatch() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let payload = b"actual file contents".to_vec();
    let claimed_hex = hex_hash_of(b"different bytes");

    let db = db
        .append_query_results([vec![upload_session(
            upload,
            vec![claimed_hex.clone()],
            false,
            false,
        )]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_part(claimed_hex, Part::bytes(payload));

    let res = server
        .post(&blobs_url(upload))
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_foreign_hash() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let payload = b"hello".to_vec();
    let payload_hex = hex_hash_of(&payload);
    let other_hex = hex_hash_of(b"other content");

    let db = db
        .append_query_results([vec![upload_session(upload, vec![other_hex], false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

    let res = server
        .post(&blobs_url(upload))
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn happy_path_uploads_blob_and_shrinks_missing() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let payload = b"a real source file".to_vec();
    let payload_hex = hex_hash_of(&payload);

    let session_row = upload_session(upload, vec![payload_hex.clone()], false, false);

    let inserted_blob = gradient_entity::build_request_blob::Model {
        id: BuildRequestBlobId::now_v7(),
        project: project_id(),
        hash: hex::decode(&payload_hex).unwrap(),
        size: payload.len() as i64,
        created_at: Utc::now().naive_utc(),
        last_used_at: Utc::now().naive_utc(),
    };
    let updated_session = gradient_entity::upload_session::Model {
        missing: json!([]),
        ..session_row.clone()
    };

    let db = db
        .append_query_results([vec![session_row]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
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
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["uploaded"], 1);
    assert_eq!(body["message"]["remaining"], 0);
}

#[tokio::test]
async fn rejects_already_dispatched_session() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let payload = b"hello".to_vec();
    let payload_hex = hex_hash_of(&payload);

    let db = db.append_query_results([vec![upload_session(
        upload,
        vec![payload_hex.clone()],
        true,
        false,
    )]]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

    let res = server
        .post(&blobs_url(upload))
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejects_expired_session() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let payload = b"hello".to_vec();
    let payload_hex = hex_hash_of(&payload);

    let db = db.append_query_results([vec![upload_session(
        upload,
        vec![payload_hex.clone()],
        false,
        true,
    )]]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_part(payload_hex, Part::bytes(payload));

    let res = server
        .post(&blobs_url(upload))
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn rejects_session_not_found() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let db = db.append_query_results([Vec::<gradient_entity::upload_session::Model>::new()]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_part("a".repeat(64), Part::bytes(b"whatever".to_vec()));

    let res = server
        .post(&blobs_url(upload))
        .add_header("authorization", auth)
        .multipart(form)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}
