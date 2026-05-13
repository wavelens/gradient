/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression tests for OIDC error handling (issue #93).
//!
//! Asserts that failures inside `oidc_login_create` / `oidc_login_verify` are
//! NOT echoed verbatim into the HTTP response body. Operators get the rich
//! error context via tracing; clients see only a stable, generic message.

use axum_test::TestServer;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::cli::OidcArgs;
use gradient_core::types::{RuntimeConfig, ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use test_support::cli::test_cli;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use web::create_router;

/// Boots a `TestServer` with OIDC enabled but pointing at an unreachable
/// discovery URL (`127.0.0.1:1` — reserved, refuses immediately).
fn server_with_broken_oidc() -> TestServer {
    let tmp = std::env::temp_dir();
    let suffix = Uuid::now_v7();

    let jwt_path = tmp.join(format!("gradient-test-jwt-{}", suffix));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let client_secret_path = tmp.join(format!("gradient-test-oidc-secret-{}", suffix));
    std::fs::write(&client_secret_path, "test-client-secret").expect("write client secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_secret_file = jwt_path.to_string_lossy().into_owned();
    cli.oidc = OidcArgs {
        oidc_enabled: true,
        oidc_required: false,
        oidc_client_id: Some("test-client".into()),
        oidc_client_secret_file: Some(client_secret_path.to_string_lossy().into_owned()),
        oidc_scopes: None,
        oidc_discovery_url: Some("http://127.0.0.1:1/oidc".into()),
    };

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new())
            as Arc<dyn gradient_core::ci::WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    });
    TestServer::new(create_router(state))
}

/// Substrings that would indicate the underlying IdP/transport error has
/// leaked into the response body. None of these must appear.
const LEAK_MARKERS: &[&str] = &[
    "Failed to fetch OIDC metadata",
    "Failed to parse OIDC metadata",
    "tcp connect",
    "connection refused",
    "127.0.0.1:1",
    "reqwest",
    "os error",
];

fn assert_no_leak(message: &str) {
    for marker in LEAK_MARKERS {
        assert!(
            !message.to_lowercase().contains(&marker.to_lowercase()),
            "response body leaks internal error detail {:?}: full message = {:?}",
            marker,
            message,
        );
    }
}

#[test]
fn oidc_login_get_does_not_leak_idp_error() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server_with_broken_oidc();
        let res = s.get("/api/v1/auth/oidc/login").await;
        res.assert_status_unauthorized();
        let body: Value = res.json();
        let msg = body["message"].as_str().expect("message string");
        assert_no_leak(msg);
    });
}

#[test]
fn oauth_authorize_post_does_not_leak_idp_error() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server_with_broken_oidc();
        let res = s.post("/api/v1/auth/oauth/authorize").await;
        res.assert_status_unauthorized();
        let body: Value = res.json();
        let msg = body["message"].as_str().expect("message string");
        assert_no_leak(msg);
    });
}

#[test]
fn oauth_authorize_get_callback_does_not_leak_idp_error() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = server_with_broken_oidc();
        // Even with code+state present, the missing CSRF cookie short-circuits
        // before we reach the IdP. That branch is already safe — this test
        // locks in that the response body shape stays stable and clean.
        let res = s
            .get("/api/v1/auth/oauth/authorize?code=abc&state=xyz")
            .await;
        res.assert_status_unauthorized();
        let body: Value = res.json();
        let msg = body["message"].as_str().expect("message string");
        assert_no_leak(msg);
    });
}
