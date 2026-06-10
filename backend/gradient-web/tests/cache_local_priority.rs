/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the local-priority override in
//! `GET /cache/{cache}/nix-cache-info`.

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::ids::*;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::net::SocketAddr;
use std::sync::Arc;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use uuid::Uuid;

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap())
}

fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000002").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn cache_row(local_priority: Option<i32>) -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 40,
        local_priority,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn build_server(cache: gradient_entity::cache::Model, peer: &str) -> TestServer {
    let cli = test_cli();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache]])
        .into_connection();

    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(
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
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
    });

    let peer_addr: SocketAddr = format!("{peer}:0").parse().expect("valid peer addr");
    let router = gradient_web::create_router(state).layer(MockConnectInfo(peer_addr));
    TestServer::new(router)
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

#[test]
fn local_priority_swapped_when_xff_in_local_ips() {
    run(async {
        let server = build_server(cache_row(Some(10)), "127.0.0.1");
        let resp = server
            .get("/cache/test-cache/nix-cache-info")
            .add_header("x-forwarded-for", "10.0.0.5")
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Priority: 10"),
            "expected Priority: 10, got:\n{body}"
        );
    });
}

#[test]
fn local_priority_not_swapped_for_non_local_xff() {
    run(async {
        let server = build_server(cache_row(Some(10)), "127.0.0.1");
        let resp = server
            .get("/cache/test-cache/nix-cache-info")
            .add_header("x-forwarded-for", "8.8.8.8")
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Priority: 40"),
            "expected Priority: 40, got:\n{body}"
        );
    });
}

#[test]
fn local_priority_null_always_uses_default() {
    run(async {
        let server = build_server(cache_row(None), "127.0.0.1");
        let resp = server
            .get("/cache/test-cache/nix-cache-info")
            .add_header("x-forwarded-for", "10.0.0.5")
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Priority: 40"),
            "expected Priority: 40, got:\n{body}"
        );
    });
}

#[test]
fn local_priority_zero_treated_as_disabled() {
    run(async {
        let server = build_server(cache_row(Some(0)), "127.0.0.1");
        let resp = server
            .get("/cache/test-cache/nix-cache-info")
            .add_header("x-forwarded-for", "10.0.0.5")
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Priority: 40"),
            "expected Priority: 40, got:\n{body}"
        );
    });
}

#[test]
fn untrusted_peer_xff_is_ignored_for_priority_decision() {
    run(async {
        let server = build_server(cache_row(Some(10)), "203.0.113.5");
        let resp = server
            .get("/cache/test-cache/nix-cache-info")
            .add_header("x-forwarded-for", "10.0.0.5")
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Priority: 40"),
            "expected Priority: 40, got:\n{body}"
        );
    });
}
