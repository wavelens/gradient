/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared test helpers for the `web` crate.
//!
//! # Prerequisites before writing handler tests
//!
//! `serve_web` builds the Axum router and immediately binds a port.
//! To make it testable, extract the router construction into a separate
//! public function in `web/src/lib.rs`:
//!
//! ```rust
//! pub fn create_router(state: Arc<ServerState>) -> axum::Router {
//!     // move the Router::new()…build() block out of serve_web
//! }
//!
//! pub async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
//!     let app = create_router(Arc::clone(&state));
//!     // … bind and serve
//! }
//! ```
//!
//! Once that exists, `server()` below will work.

use anyhow::Result;
use axum_test::TestServer;
use core::log_storage::LogStorage;
use core::pool::NixStorePool;
use core::types::{Cli, ServerState};
use entity::*;
use futures::future::BoxFuture;
use sea_orm::{DatabaseBackend, DatabaseConnection, IntoMockRow, MockDatabase};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug)]
struct NoopLogStorage;

impl LogStorage for NoopLogStorage {
    fn append<'a>(&'a self, _build_id: Uuid, _text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn read<'a>(&'a self, _build_id: Uuid) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
}

// ── State helpers ─────────────────────────────────────────────────────────────

/// Single source of truth for the `Cli` struct in tests.
/// Update only here when fields are added/removed from `Cli`.
pub fn test_cli() -> Cli {
    Cli {
        log_level: "error".into(),
        ip: "127.0.0.1".into(),
        port: 3000,
        serve_url: "http://127.0.0.1:3000".into(),
        database_url: None,
        database_url_file: None,
        max_concurrent_evaluations: 2,
        max_concurrent_builds: 10,
        evaluation_timeout: 5,
        store_path: None,
        base_path: "/tmp/gradient-test".into(),
        enable_registration: false,
        oidc_enabled: false,
        oidc_required: false,
        oidc_client_id: None,
        oidc_client_secret_file: None,
        oidc_scopes: None,
        oidc_discovery_url: None,
        crypt_secret_file: "test-secret".into(),
        jwt_secret_file: "test-jwt".into(),
        serve_cache: false,
        binpath_nix: "nix".into(),
        binpath_ssh: "ssh".into(),
        report_errors: false,
        email_enabled: false,
        email_require_verification: false,
        email_smtp_host: None,
        email_smtp_port: 587,
        email_smtp_username: None,
        email_smtp_password_file: None,
        email_from_address: None,
        email_from_name: "Gradient Test".into(),
        email_enable_tls: false,
        state_file: None,
        delete_state: true,
        keep_evaluations: 30,
        max_nixdaemon_connections: 2,
        nar_ttl_hours: 0,
    }
}

pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    Arc::new(ServerState {
        db,
        cli: test_cli(),
        log_storage: Arc::new(NoopLogStorage),
        nix_store_pool: NixStorePool::new(1),
        web_nix_store_pool: NixStorePool::new(1),
    })
}

// ── DB helpers ────────────────────────────────────────────────────────────────

/// MockDatabase that answers the next SELECT with `rows`.
pub fn db_with<T: IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}

// ── Router helper ─────────────────────────────────────────────────────────────

/// Spin up a `TestServer` backed by the real Axum router and a mocked DB.
///
/// Requires `pub fn create_router(state: Arc<ServerState>) -> axum::Router`
/// to be exported from `web/src/lib.rs`. See module-level doc comment.
#[allow(dead_code)]
pub async fn server(db: DatabaseConnection) -> TestServer {
    // TODO: uncomment once create_router is extracted from serve_web
    // let app = web::create_router(test_state(db));
    // TestServer::new(app).unwrap()
    let _ = db;
    unimplemented!("extract create_router() from serve_web() first — see TESTING.md")
}

// ── Fixture builders ──────────────────────────────────────────────────────────
//
// Deterministic values make assertions readable: you can write
//   assert_eq!(body["name"], "test-org")
// instead of tracking a random Uuid.

pub fn org_id() -> Uuid { Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap() }
pub fn user_id() -> Uuid { Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap() }
pub fn project_id() -> Uuid { Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap() }

pub fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

pub fn org() -> organization::Model {
    organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Organization".into(),
        description: "".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        use_nix_store: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
    }
}

pub fn user() -> user::Model {
    user::Model {
        id: user_id(),
        username: "testuser".into(),
        name: "Test User".into(),
        email: "test@example.com".into(),
        password: Some(password_auth::generate_hash("TestPass123!")),
        last_login_at: test_date(),
        created_at: test_date(),
    }
}
