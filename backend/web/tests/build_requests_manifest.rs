/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/manifest` (issue #234).
//!
//! Covers the four validation surfaces (oversized total, bad paths, bad
//! hashes, duplicates) and the happy-path response shape — `session` is a
//! UUID and `missing` is the subset of hex hashes the org doesn't have yet.

use axum::http::StatusCode;
use entity::ids::*;
use entity::role;
use gradient_core::permissions::PermissionMask;
use gradient_core::types::SessionId;
use gradient_core::types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use test_support::fixtures::{org, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

const URL: &str = "/api/v1/build-requests/manifest";

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
        organization: test_support::fixtures::org_id(),
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

fn with_org_access(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![membership()]])
        .append_query_results([vec![write_role_row()]])
}

fn hex_hash(byte: u8) -> String {
    let mut s = String::with_capacity(64);
    for _ in 0..32 {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn rejects_oversized_total() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_access(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));
        let server = make_test_server(db.into_connection());

        // 20 MiB + 1 byte triggers the cap.
        let body = json!({
            "organization": "test-org",
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
    });
}

#[test]
fn rejects_bad_paths() {
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
        run(async move {
            let session_id = SessionId::now_v7();
            let token = make_token(session_id);

            let db = with_org_access(with_auth(
                MockDatabase::new(DatabaseBackend::Postgres),
                session_id,
            ));
            let server = make_test_server(db.into_connection());

            let body = json!({
                "organization": "test-org",
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
        });
    }
}

#[test]
fn rejects_bad_hashes() {
    let bad_hashes = [
        "".to_string(),
        "abc".to_string(),
        "g".repeat(64),
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ];

    for hash in bad_hashes {
        run(async {
            let session_id = SessionId::now_v7();
            let token = make_token(session_id);

            let db = with_org_access(with_auth(
                MockDatabase::new(DatabaseBackend::Postgres),
                session_id,
            ));
            let server = make_test_server(db.into_connection());

            let body = json!({
                "organization": "test-org",
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
        });
    }
}

#[test]
fn rejects_duplicate_paths() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_access(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));
        let server = make_test_server(db.into_connection());

        let body = json!({
            "organization": "test-org",
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
    });
}

#[test]
fn happy_path_returns_session_and_missing() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let now_ts = chrono::Utc::now().naive_utc();
        let inserted_session = entity::upload_session::Model {
            id: UploadSessionId::now_v7(),
            organization: test_support::fixtures::org_id(),
            manifest: json!([]),
            missing: json!([]),
            total_size: 300,
            created_at: now_ts,
            expires_at: now_ts + chrono::Duration::hours(1),
            dispatched_at: None,
        };

        // After auth+org access, the handler runs:
        //   SELECT build_request_blob WHERE org=... AND hash IN (...) → empty
        //   INSERT upload_session  (RETURNING + rows_affected)
        let db = with_org_access(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<entity::build_request_blob::Model>::new()])
        .append_query_results([vec![inserted_session]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());

        let hash_a = hex_hash(0xaa);
        let hash_b = hex_hash(0xbb);
        let body = json!({
            "organization": "test-org",
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

        let missing = body["message"]["missing"].as_array().expect("missing array");
        let missing_set: std::collections::HashSet<&str> =
            missing.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(missing.len(), 2);
        assert!(missing_set.contains(hash_a.as_str()));
        assert!(missing_set.contains(hash_b.as_str()));
    });
}
