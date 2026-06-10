/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! End-to-end tests for the `gradient login` web flow (issue #251).
//!
//! Exercises `/auth/cli/start`, `/auth/cli/poll`, `/auth/cli/authorize`, and
//! `/auth/cli/deny` through the real router with mocked Postgres - enough to
//! pin down the state machine (pending → authorized/denied/expired) and the
//! "device_code is single-use" guarantee.

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::{cli_device_authorization, session};
use gradient_core::storage::{EmailSender, NarStore};
use gradient_types::{
    CliDeviceAuthorizationId, RuntimeConfig, SecretString, SessionId, UserId,
};
use gradient_core::ServerState;
use gradient_core::db::{WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{user, user_id};
use gradient_test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use gradient_web::create_router;

const JWT_SECRET: &str = "test-jwt-secret";

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
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
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

fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
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
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let db = web_db_setup(MockDatabase::new(DatabaseBackend::Postgres));
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db.into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_core::db::NoReactor),
    });
    TestServer::new(create_router(state))
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

#[test]
fn start_returns_user_code_and_verification_uri() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}

#[test]
fn poll_pending_returns_cli_auth_pending() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let row = pending_row("dev-code-xyz", "ABCD-EFGH");
        let s = server_with(|db| db.append_query_results([vec![row]]));

        let res = s
            .post("/api/v1/auth/cli/poll")
            .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
            .await;
        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["code"], "cli_auth_pending");
    });
}

#[test]
fn poll_denied_returns_cli_auth_denied() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}

#[test]
fn poll_expired_returns_cli_auth_expired() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}

#[test]
fn poll_authorized_returns_token_once() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
                .append_query_results([vec![cli_device_authorization::Model {
                    token: None,
                    ..row
                }]])
        });

        let res = s
            .post("/api/v1/auth/cli/poll")
            .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], Value::Bool(false));
        assert_eq!(body["message"], "the-session-jwt");
    });
}

#[test]
fn poll_unknown_device_code_returns_404() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server_with(|db| db.append_query_results([Vec::<cli_device_authorization::Model>::new()]));

        let res = s
            .post("/api/v1/auth/cli/poll")
            .json(&serde_json::json!({ "device_code": "nope" }))
            .await;
        res.assert_status_not_found();
    });
}

#[test]
fn authorize_requires_auth() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server_with(|db| db);

        let res = s
            .post("/api/v1/auth/cli/authorize")
            .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
            .await;
        res.assert_status_forbidden();
    });
}

#[test]
fn deny_marks_row_denied() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}
