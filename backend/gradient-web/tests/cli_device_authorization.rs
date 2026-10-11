/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::{cli_device_authorization, session};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, user};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_types::{CliDeviceAuthorizationId, SessionId, UserId};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

fn sign_session_jwt(user_id: UserId, session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(1)).timestamp() as usize,
        id: user_id,
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

fn hash_device_code(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn auth_queue(db: MockDatabase, session: session::Model) -> MockDatabase {
    db.append_query_results([vec![session.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn server_with(web_db_setup: impl FnOnce(MockDatabase) -> MockDatabase) -> TestServer {
    TestState::new()
        .web_db(web_db_setup(mock_db()).into_connection())
        .server()
}

fn pending_row(device_code: &str, user_code: &str) -> cli_device_authorization::Model {
    let now = Utc::now().naive_utc();
    cli_device_authorization::Model {
        id: CliDeviceAuthorizationId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000c1").unwrap(),
        ),
        device_code_hash: hash_device_code(device_code),
        user_code: user_code.to_string(),
        created_at: now,
        expires_at: now + Duration::minutes(10),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn start_returns_user_code_and_verification_uri() {
    let inserted = pending_row("dev-code", "ABCD-EFGH");
    let s = server_with(|db| {
        db.append_query_results([Vec::<cli_device_authorization::Model>::new()])
            .append_query_results([vec![inserted]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
    });

    let res = s.post("/api/v1/auth/cli/start").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(false));
    let m = &body["message"];
    assert!(m["device_code"].as_str().unwrap().len() >= 32);
    assert!(m["user_code"].as_str().unwrap().contains('-'));
    assert!(
        m["verification_uri_complete"]
            .as_str()
            .unwrap()
            .contains("/account/cli-authorize?code=")
    );
    assert!(m["interval"].as_u64().unwrap() >= 1);
    assert!(m["expires_in"].as_i64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_pending_returns_cli_auth_pending() {
    let row = pending_row("dev-code-xyz", "ABCD-EFGH");
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_denied_returns_cli_auth_denied() {
    let mut row = pending_row("dev-code-xyz", "ABCD-EFGH");
    row.denied_at = Some(Utc::now().naive_utc());
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_denied");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_expired_returns_cli_auth_expired() {
    let mut row = pending_row("dev-code-xyz", "ABCD-EFGH");
    row.expires_at = Utc::now().naive_utc() - Duration::seconds(1);
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_expired");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_authorized_returns_token_once() {
    let mut row = pending_row("dev-code-xyz", "ABCD-EFGH");
    row.user_id = Some(user_id());
    row.token = Some("the-session-jwt".to_string());
    row.authorized_at = Some(Utc::now().naive_utc());
    let s = server_with(|db| {
        db.append_query_results([vec![row.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![cli_device_authorization::Model { token: None, ..row }]])
    });

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(false));
    assert_eq!(body["message"], "the-session-jwt");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_unknown_device_code_returns_404() {
    let s =
        server_with(|db| db.append_query_results([Vec::<cli_device_authorization::Model>::new()]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "nope" }))
        .await;
    res.assert_status_not_found();
}

#[tokio::test(flavor = "multi_thread")]
async fn authorize_requires_auth() {
    let s = server_with(|db| db);

    let res = s
        .post("/api/v1/auth/cli/authorize")
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await;
    res.assert_status_forbidden();
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_marks_row_denied() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id);
    let row = pending_row("dev-code-xyz", "ABCD-EFGH");

    let s = server_with(|db| {
        let db = auth_queue(db, session);
        db.append_query_results([vec![row.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![cli_device_authorization::Model {
                denied_at: Some(Utc::now().naive_utc()),
                ..row
            }]])
            .append_query_results([Vec::<gradient_entity::audit_log::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
    });

    let res = s
        .post("/api/v1/auth/cli/deny")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await;
    res.assert_status_ok();
}
