/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{project, project_member, write_role};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

const URL: &str = "/api/v1/build-requests/manifest";

fn with_project_access(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
}

fn hex_hash(byte: u8) -> String {
    let mut s = String::with_capacity(64);
    for _ in 0..32 {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

#[tokio::test]
async fn rejects_oversized_total() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_access(with_auth(mock_db(), session_id));
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.http.max_source_upload_size =
                std::num::NonZeroUsize::new(20 * 1024 * 1024).expect("non-zero");
        })
        .server();

    let body = json!({
        "project": "test-project",
        "files": [
            {"path": "a", "hash": hex_hash(0xaa), "size": 20 * 1024 * 1024 + 1i64 },
        ]
    });

    let res = server
        .post(URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&body)
        .await;

    res.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert_eq!(body["code"], "payload_too_large");
}

#[tokio::test]
async fn rejects_bad_paths() {
    let bad_paths = [
        "../escape",
        "/absolute",
        ".",
        "with\0null",
        "a/./b",
        "",
        "foo/../bar",
    ];

    for path in bad_paths {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_access(with_auth(mock_db(), session_id));
        let server = make_test_server(db.into_connection());

        let body = json!({
            "project": "test-project",
            "files": [
                {"path": path, "hash": hex_hash(0xaa), "size": 1i64 },
            ]
        });

        let res = server
            .post(URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&body)
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    }
}

#[tokio::test]
async fn rejects_bad_hashes() {
    let bad_hashes = [
        "".to_string(),
        "abc".to_string(),
        "g".repeat(64),
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ];

    for hash in bad_hashes {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_access(with_auth(mock_db(), session_id));
        let server = make_test_server(db.into_connection());

        let body = json!({
            "project": "test-project",
            "files": [
                {"path": "foo.txt", "hash": hash, "size": 1i64 },
            ]
        });

        let res = server
            .post(URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&body)
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    }
}

#[tokio::test]
async fn rejects_duplicate_paths() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_access(with_auth(mock_db(), session_id));
    let server = make_test_server(db.into_connection());

    let body = json!({
        "project": "test-project",
        "files": [
            {"path": "a", "hash": hex_hash(0xaa), "size": 1i64 },
            {"path": "a", "hash": hex_hash(0xbb), "size": 1i64 },
        ]
    });

    let res = server
        .post(URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&body)
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn happy_path_returns_session_and_missing() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let now_ts = chrono::Utc::now().naive_utc();
    let inserted_session = gradient_entity::upload_session::Model {
        id: UploadSessionId::now_v7(),
        project: gradient_fixtures::ids::project_id(),
        manifest: json!([]),
        missing: json!([]),
        total_size: 300,
        created_at: now_ts,
        expires_at: now_ts + chrono::Duration::hours(1),
        ..Default::default()
    };

    let db = with_project_access(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<gradient_entity::build_request_blob::Model>::new()])
        .append_query_results([vec![inserted_session]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());

    let hash_a = hex_hash(0xaa);
    let hash_b = hex_hash(0xbb);
    let body = json!({
        "project": "test-project",
        "files": [
            {"path": "flake.nix", "hash": hash_a, "size": 100i64 },
            {"path": "src/main.rs", "hash": hash_b, "size": 200i64 },
        ]
    });

    let res = server
        .post(URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&body)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);

    let session_str = body["message"]["session"].as_str().expect("session str");
    assert!(
        Uuid::parse_str(session_str).is_ok(),
        "session is not a UUID: {session_str}"
    );

    let missing = body["message"]["missing"]
        .as_array()
        .expect("missing array");
    let missing_set: std::collections::HashSet<&str> =
        missing.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(missing.len(), 2);
    assert!(missing_set.contains(hash_a.as_str()));
    assert!(missing_set.contains(hash_b.as_str()));
}
