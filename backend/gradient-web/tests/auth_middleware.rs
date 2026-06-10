/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Wire-format regression tests for `authorization::authorize` middleware.
//!
//! Locks in the `BaseResponse<String>` envelope and HTTP status codes so the
//! refactor that routes middleware errors through `WebError::IntoResponse`
//! stays observably equivalent to the prior hand-built responses.

use axum_test::TestServer;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::{RuntimeConfig};
use gradient_core::ServerState;
use gradient_core::db::{WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use gradient_web::create_router;

/// Build a `ServerState` whose `jwt_secret_file` points at a real on-disk
/// file - required because `load_secret` calls `process::exit(1)` if the
/// file is missing, which would tear down the test process before assertions
/// run.
fn server() -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-test-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_secret_file = jwt_path.to_string_lossy().into_owned();

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_core::db::NoReactor),
    });
    TestServer::new(create_router(state))
}

fn assert_envelope(body: &Value, expected_message: &str) {
    assert_eq!(body["error"], Value::Bool(true), "error flag must be true");
    assert_eq!(
        body["message"],
        Value::String(expected_message.to_string()),
        "message must match"
    );
}

#[test]
fn missing_auth_header_returns_403_envelope() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server();
        let res = s.get("/api/v1/user").await;
        res.assert_status_forbidden();
        assert_envelope(&res.json::<Value>(), "Authorization header not found");
    });
}

#[test]
fn malformed_bearer_returns_403_envelope() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server();
        let res = s
            .get("/api/v1/user")
            .add_header("authorization", "NotBearer xyz")
            .await;
        res.assert_status_forbidden();
        assert_envelope(&res.json::<Value>(), "Invalid Authorization header");
    });
}

#[test]
fn undecodable_token_returns_401_envelope() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server();
        let res = s
            .get("/api/v1/user")
            .add_header("authorization", "Bearer not-a-real-jwt")
            .await;
        res.assert_status_unauthorized();
        assert_envelope(&res.json::<Value>(), "Unable to decode token");
    });
}
