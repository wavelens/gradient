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
use gradient_entity::{api, session};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, user};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_types::{ApiId, ConcurrencyPolicy, SessionId, UserId};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Serialize)]
struct Claims {
    exp: i64,
    iat: i64,
    id: UserId,
    jti: SessionId,
}

fn sign_session_jwt(user_id: UserId, session_id: SessionId, lifetime: Duration) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp(),
        exp: (now + lifetime).timestamp(),
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

fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn server_with(web_db_setup: impl FnOnce(MockDatabase) -> MockDatabase) -> TestServer {
    TestState::new()
        .web_db(web_db_setup(mock_db()).into_connection())
        .server()
}

fn revoked_session() -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id: SessionId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        user_id: user_id(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(1),
        last_used_at: now,
        revoked_at: Some(now),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_revoked_session_is_rejected() {
    let session = revoked_session();
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| db.append_query_results([vec![session]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_unknown_session_is_rejected() {
    let token = sign_session_jwt(user_id(), SessionId::now_v7(), Duration::hours(1));

    let s = server_with(|db| db.append_query_results([Vec::<session::Model>::new()]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_expired_session_is_rejected() {
    let now = Utc::now().naive_utc();
    let mut session = live_session(SessionId::now_v7());
    session.expires_at = now - chrono::Duration::seconds(1);
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| db.append_query_results([vec![session]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_api_key_is_rejected() {
    let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "leaked".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        revoked_at: Some(now),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| db.append_query_results([vec![key]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_api_key_is_rejected() {
    let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "expired".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        expires_at: Some(now - chrono::Duration::seconds(1)),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| db.append_query_results([vec![key]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status_unauthorized();
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

#[tokio::test(flavor = "multi_thread")]
async fn delete_user_without_password_is_forbidden() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| auth_queue(db, session));

    let res = s
        .delete("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({}))
        .await;
    res.assert_status_forbidden();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_user_with_wrong_password_is_forbidden() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| auth_queue(db, session));

    let res = s
        .delete("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "password": "WrongPassword!" }))
        .await;
    res.assert_status_forbidden();
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_with_only_view_cannot_trigger_evaluation() {
    use gradient_fixtures::ids::project_id;
    use gradient_fixtures::rows::project;
    use gradient_types::permissions::{Permission, mask_from};
    let raw = "x".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "ci".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(&[Permission::ViewProject]),
        ..Default::default()
    };
    let admin_membership = gradient_entity::project_user::Model {
        id: gradient_entity::ids::ProjectUserId::now_v7(),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
    };
    let admin_role = gradient_entity::role::Model {
        id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
            .append_query_results([vec![project()]])
            .append_query_results([vec![gradient_entity::task::Model {
                id: gradient_fixtures::ids::task_id(),
                project: project_id(),
                name: "test-task".into(),
                display_name: "Test".into(),
                repository: "git@example.com:test/test.git".into(),
                wildcard: "*".into(),
                active: true,
                last_check_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
                created_by: user_id(),
                created_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
                keep_evaluations: 30,
                concurrency: ConcurrencyPolicy::Skip,
                sign_cache: true,
                ..Default::default()
            }]])
            .append_query_results([vec![admin_membership]])
            .append_query_results([vec![admin_role]])
    });

    let res = s
        .post("/api/v1/tasks/test-project/test-task/evaluate")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_pinned_to_other_project_is_invisible() {
    use gradient_fixtures::rows::project;
    use gradient_types::permissions::{Permission, mask_from};
    let raw = "y".repeat(64);
    let now = Utc::now().naive_utc();
    let pinned_elsewhere =
        gradient_entity::ids::ProjectId::new(uuid::uuid!("ffffffff-ffff-ffff-ffff-ffffffffffff"));
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "ci".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(Permission::ALL),
        project: Some(pinned_elsewhere),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
            .append_query_results([vec![project()]])
    });

    let res = s
        .get("/api/v1/projects/test-project")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_cannot_create_api_keys() {
    let raw = "z".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "self".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
    });

    let res = s
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .json(&serde_json::json!({
            "name": "child",
            "permissions": ["viewProject"],
        }))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_cannot_add_ssh_keys() {
    let raw = "y".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "self".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
    });

    let res = s
        .post("/api/v1/user/ssh-keys")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .json(&serde_json::json!({
            "name": "laptop",
            "public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test",
        }))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}
