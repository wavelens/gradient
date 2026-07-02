/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared `ServerState` builder for `cacher` unit tests. Collapses the
//! hand-built `ServerState { ... }` literals that used to be duplicated
//! across `cleanup.rs` and `deep_gc.rs` test modules.

use gradient_core::ServerState;
use gradient_db::{CacheDb, WebDb, WorkerDb};
use gradient_storage::{EmailSender, LogStorage, NarStore};
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use gradient_types::RuntimeConfig;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use std::sync::Arc;

/// Builds a `ServerState` for a cacher test. `nar`/`db` carry the
/// test-specific storage and mock DB wiring; `configure` mutates the default
/// `test_cli()` config (e.g. `nar_ttl_hours`, `nar_upload_grace_hours`)
/// before it's wrapped in the returned `Arc`. `log_storage` is `NoopLogStorage`;
/// use [`test_server_state_with_log`] when a test needs a real one.
pub(crate) fn test_server_state(
    nar: NarStore,
    db: DatabaseConnection,
    configure: impl FnOnce(&mut RuntimeConfig),
) -> Arc<ServerState> {
    test_server_state_with_log(nar, Arc::new(NoopLogStorage), db, configure)
}

pub(crate) fn test_server_state_with_log(
    nar: NarStore,
    log_storage: Arc<dyn LogStorage>,
    db: DatabaseConnection,
    configure: impl FnOnce(&mut RuntimeConfig),
) -> Arc<ServerState> {
    let mut config = RuntimeConfig::from_cli(&test_cli()).expect("valid test config");
    configure(&mut config);

    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        cache_db: CacheDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(db),
        config: Arc::new(config),
        log_storage,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage: nar,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        scim_group_roles: Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        upstream_query: Arc::new(tokio::sync::Semaphore::new(32)),
        reactor: Arc::new(gradient_db::NoReactor),
    })
}
