/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::RuntimeConfig;
use gradient_types::cli::OidcArgs;
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

/// `127.0.0.1:1` is reserved and is refusing connections immediately.
fn server_with_broken_oidc() -> TestServer {
    let tmp = std::env::temp_dir();
    let suffix = Uuid::now_v7();

    let jwt_path = tmp.join(format!("gradient-test-jwt-{}", suffix));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let client_secret_path = tmp.join(format!("gradient-test-oidc-secret-{}", suffix));
    std::fs::write(&client_secret_path, "test-client-secret").expect("write client secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned();
    cli.oidc = OidcArgs {
        enable: true,
        required: false,
        client_id: Some("test-client".into()),
        client_secret_file: Some(client_secret_path.to_string_lossy().into_owned()),
        scopes: None,
        discovery_url: Some("http://127.0.0.1:1/oidc".into()),
    };

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    });
    TestServer::new(create_router(state).expect("router"))
}

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

#[tokio::test(flavor = "multi_thread")]
async fn oidc_login_get_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s.get("/api/v1/auth/oidc/login").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_authorize_post_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s.post("/api/v1/auth/oauth/authorize").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_authorize_get_callback_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s
        .get("/api/v1/auth/oauth/authorize?code=abc&state=xyz")
        .await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}
