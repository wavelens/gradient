/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared test helpers for the `core` crate.
//!
//! Never spell out `Cli` fields in individual test files — use `test_cli()` here.
//! When a new field is added to `Cli`, update exactly this one place.

extern crate core as gradient_core;

use anyhow::Result;
use futures::future::BoxFuture;
use gradient_core::log_storage::LogStorage;
use gradient_core::pool::NixStorePool;
use gradient_core::types::{Cli, ServerState};
use sea_orm::{DatabaseBackend, DatabaseConnection, IntoMockRow, MockDatabase};
use std::sync::Arc;
use uuid::Uuid;

/// Minimal no-op log storage for tests.
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

/// A `Cli` with safe defaults for tests.
/// All paths are set to values that don't require real files.
/// `log_level` is "error" to suppress noise in test output.
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

/// Wrap a `DatabaseConnection` + `test_cli()` into a `ServerState`.
pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    Arc::new(ServerState {
        db,
        cli: test_cli(),
        log_storage: Arc::new(NoopLogStorage),
        nix_store_pool: NixStorePool::new(1),
        web_nix_store_pool: NixStorePool::new(1),
    })
}

/// A `MockDatabase` that answers the next `SELECT` with `rows`.
/// Chain `.append_query_results` manually when a handler makes multiple queries.
pub fn db_with<T: IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}
