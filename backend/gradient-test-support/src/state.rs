/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::log_storage::NoopLogStorage;
use gradient_core::ServerState;
use gradient_db::{CacheDb, WebDb, WorkerDb};
use gradient_notify::EmailSender;
use gradient_storage::LogStorage;
use gradient_storage::NarStore;
use gradient_types::{RuntimeConfig, SecretString};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use std::sync::Arc;

fn empty_mock() -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres).into_connection()
}

pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    let nar_storage = NarStore::local(&config().server.base_dir).expect("create test NarStore");
    test_state_with_storage(db, nar_storage)
}

pub fn test_state_with_storage(db: DatabaseConnection, nar_storage: NarStore) -> Arc<ServerState> {
    assemble(
        Pools {
            worker: db,
            ..Pools::empty()
        },
        nar_storage,
        Arc::new(NoopLogStorage),
    )
}

/// The `CacheQuery` handler is reading the dedicated `cache_db` pool, not `db`.
pub fn test_state_cache(db: DatabaseConnection) -> Arc<ServerState> {
    with_pools(Pools {
        cache: db,
        ..Pools::empty()
    })
}

pub fn test_state_web(db: DatabaseConnection) -> Arc<ServerState> {
    with_pools(Pools {
        web: db,
        ..Pools::empty()
    })
}

pub fn test_state_with_log_storage(
    db: DatabaseConnection,
    log_storage: Arc<dyn LogStorage>,
) -> Arc<ServerState> {
    let nar_storage = NarStore::local(&config().server.base_dir).expect("create test NarStore");
    assemble(
        Pools {
            worker: db,
            ..Pools::empty()
        },
        nar_storage,
        log_storage,
    )
}

struct Pools {
    web: DatabaseConnection,
    cache: DatabaseConnection,
    worker: DatabaseConnection,
}

impl Pools {
    fn empty() -> Self {
        Self {
            web: empty_mock(),
            cache: empty_mock(),
            worker: empty_mock(),
        }
    }
}

fn config() -> RuntimeConfig {
    RuntimeConfig::from_cli(&test_cli()).expect("valid test config")
}

fn with_pools(pools: Pools) -> Arc<ServerState> {
    let nar_storage = NarStore::local(&config().server.base_dir).expect("create test NarStore");
    assemble(pools, nar_storage, Arc::new(NoopLogStorage))
}

fn assemble(
    pools: Pools,
    nar_storage: NarStore,
    log_storage: Arc<dyn LogStorage>,
) -> Arc<ServerState> {
    Arc::new(ServerState {
        web_db: WebDb::new(pools.web),
        cache_db: CacheDb::new(pools.cache),
        worker_db: WorkerDb::new(pools.worker),
        config: Arc::new(config()),
        log_storage,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("build test HTTP client"),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: Arc::new(std::collections::HashMap::new()),
        events: gradient_types::EventBus::default(),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
    })
}
