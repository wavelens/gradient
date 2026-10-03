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
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;

fn make_state() -> Arc<ServerState> {
    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: std::sync::Arc::new(
            gradient_types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
        ),
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        download_progress: gradient_core::download_progress(),
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
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    })
}

#[tokio::test]
async fn auth_tier_throttles_burst() {
    let server = TestServer::new(create_router(make_state()).expect("router"));

    for i in 1..=5 {
        let resp = server
            .post("/api/v1/auth/check-username")
            .json(&serde_json::json!({"username": "x"}))
            .await;
        assert_eq!(
            resp.status_code(),
            200,
            "request {} unexpectedly throttled: {:?}",
            i,
            resp.status_code()
        );
    }

    let throttled = server
        .post("/api/v1/auth/check-username")
        .json(&serde_json::json!({"username": "x"}))
        .await;
    assert_eq!(
        throttled.status_code(),
        429,
        "6th burst request should be 429, got {:?}",
        throttled.status_code()
    );
}

#[tokio::test]
async fn cache_tier_does_not_throttle_moderate_burst() {
    let server = TestServer::new(create_router(make_state()).expect("router"));

    for i in 1..=50 {
        let resp = server.get("/cache/missing-cache/nix-cache-info").await;
        assert_ne!(
            resp.status_code(),
            429,
            "cache request {} unexpectedly throttled",
            i
        );
    }
}

#[tokio::test]
async fn cache_proto_tier_does_not_throttle_burst() {
    let server = TestServer::new(create_router(make_state()).expect("router"));

    for i in 1..=250 {
        let resp = server.get("/cache/missing-cache/proto").await;
        assert_ne!(
            resp.status_code(),
            429,
            "cache proto request {} unexpectedly throttled",
            i
        );
    }
}
