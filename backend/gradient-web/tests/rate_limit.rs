/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the per-IP HTTP rate limiter.
//!
//! Verifies that the sensitive auth tier rejects bursts beyond its capacity
//! with HTTP 429, while the public NAR cache tier (`/cache/{cache}/...`) is
//! sized generously enough that substituters issuing many requests per build
//! aren't throttled at moderate burst.

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_storage::{EmailSender, NarStore};
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;

fn make_state() -> Arc<ServerState> {
    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
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
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
    })
}

/// Auth tier burst is 5: requests 1-5 from the same client succeed, request
/// 6 is rejected with 429 before the handler runs. We use
/// `/api/v1/auth/check-username` with a too-short username so the handler
/// returns early without touching the DB.
#[test]
fn auth_tier_throttles_burst() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
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
    });
}

/// Cache tier burst is 1000: 50 rapid GETs against `/cache/{cache}/...` all
/// succeed (or fail through to the handler) - no 429s. The handler itself
/// 404s for the unknown cache, but the request reaching the handler proves
/// the rate limiter didn't reject it.
#[test]
fn cache_tier_does_not_throttle_moderate_burst() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
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
    });
}

/// The cache-scoped proto WS upgrade shares the generous NAR-download tier
/// (burst 3000), so 250 rapid upgrade attempts against `/cache/{cache}/proto`
/// are never rejected with 429. The handler rejects the non-upgrade GET
/// downstream, but reaching it proves the per-IP limiter let the request pass.
#[test]
fn cache_proto_tier_does_not_throttle_burst() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
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
    });
}
