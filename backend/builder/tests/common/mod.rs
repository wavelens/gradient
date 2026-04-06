/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared test helpers for the `builder` crate.

use anyhow::Result;
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

pub fn db_with<T: IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}

// ── Fixture builders ──────────────────────────────────────────────────────────

pub fn project_id() -> Uuid { Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap() }
pub fn org_id() -> Uuid     { Uuid::parse_str("00000000-0000-0000-0000-000000000011").unwrap() }
pub fn user_id() -> Uuid    { Uuid::parse_str("00000000-0000-0000-0000-000000000012").unwrap() }
pub fn commit_id() -> Uuid  { Uuid::parse_str("00000000-0000-0000-0000-000000000013").unwrap() }

pub fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

pub fn eval_at(id: Uuid, offset_secs: i64) -> evaluation::Model {
    let created_at = test_date() + chrono::Duration::seconds(offset_secs);
    evaluation::Model {
        id,
        project: project_id(),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status: evaluation::EvaluationStatus::Completed,
        previous: None,
        next: None,
        created_at,
    }
}
