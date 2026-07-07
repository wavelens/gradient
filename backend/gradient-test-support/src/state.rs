/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::log_storage::NoopLogStorage;
use gradient_storage::EmailSender;
use gradient_storage::LogStorage;
use gradient_storage::NarStore;
use gradient_types::{RuntimeConfig, SecretString};
use gradient_core::ServerState;
use gradient_db::{CacheDb, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use std::sync::Arc;

fn empty_mock() -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres).into_connection()
}

/// Wrap a `DatabaseConnection` + `test_cli()` into a `ServerState`.
///
/// Tests that don't exercise the web layer get an empty mock for `web_db`.
/// Tests that need a populated web pool should construct `ServerState`
/// directly so they can supply their own mock query results.
pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(empty_mock()),
        cache_db: CacheDb::new(empty_mock()),
        worker_db: WorkerDb::new(db),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("build test HTTP client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        scim_group_roles: Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Like `test_state` but routes `db` into the dedicated cache-query pool
/// (`cache_db`), which is what the `CacheQuery` handler reads. Use this to drive
/// cache-lookup tests (including injecting DB errors via `append_query_errors`).
pub fn test_state_cache(db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(empty_mock()),
        cache_db: CacheDb::new(db),
        worker_db: WorkerDb::new(empty_mock()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("build test HTTP client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        scim_group_roles: Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Like `test_state` but plumbs through a caller-supplied [`LogStorage`].
pub fn test_state_with_log_storage(
    db: DatabaseConnection,
    log_storage: Arc<dyn LogStorage>,
) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(empty_mock()),
        cache_db: CacheDb::new(empty_mock()),
        worker_db: WorkerDb::new(db),
        config,
        log_storage,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("build test HTTP client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        scim_group_roles: Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}
