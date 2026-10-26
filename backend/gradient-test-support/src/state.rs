/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::log_storage::NoopLogStorage;
use axum_test::TestServer;
use chrono::{DateTime, Utc};
use gradient_core::{Graph, InstanceMemory, ServerExtensions, ServerState};
use gradient_db::{CacheDb, WebDb, WorkerDb};
use gradient_entity::StorePathMapping;
use gradient_fixtures::mock::empty_db;
use gradient_notify::EmailSender;
use gradient_storage::{LogStorage, NarStore};
use gradient_types::{Cli, RuntimeConfig, SecretString};
use sea_orm::DatabaseConnection;
use std::sync::Arc;

pub const TEST_JWT_SECRET: &str = "test-jwt-secret";

type ConfigChange = Box<dyn FnOnce(&mut RuntimeConfig)>;

pub struct TestState {
    cli: Cli,
    config_changes: Vec<ConfigChange>,
    web_db: DatabaseConnection,
    cache_db: DatabaseConnection,
    worker_db: DatabaseConnection,
    nar_storage: Option<NarStore>,
    log_storage: Arc<dyn LogStorage>,
    email: Arc<dyn EmailSender>,
    graph: Option<Arc<Graph>>,
    started_at: Option<DateTime<Utc>>,
    extensions: ServerExtensions,
}

impl Default for TestState {
    fn default() -> Self {
        Self {
            cli: test_cli(),
            config_changes: Vec::new(),
            web_db: empty_db(),
            cache_db: empty_db(),
            worker_db: empty_db(),
            nar_storage: None,
            log_storage: Arc::new(NoopLogStorage),
            email: Arc::new(InMemoryEmailSender::new()),
            graph: None,
            started_at: None,
            extensions: ServerExtensions::default(),
        }
    }
}

impl TestState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn web_db(mut self, db: DatabaseConnection) -> Self {
        self.web_db = db;
        self
    }

    pub fn cache_db(mut self, db: DatabaseConnection) -> Self {
        self.cache_db = db;
        self
    }

    pub fn worker_db(mut self, db: DatabaseConnection) -> Self {
        self.worker_db = db;
        self
    }

    pub fn cli(mut self, change: impl FnOnce(&mut Cli)) -> Self {
        change(&mut self.cli);
        self
    }

    pub fn config(mut self, change: impl FnOnce(&mut RuntimeConfig) + 'static) -> Self {
        self.config_changes.push(Box::new(change));
        self
    }

    pub fn nar_storage(mut self, nar_storage: NarStore) -> Self {
        self.nar_storage = Some(nar_storage);
        self
    }

    pub fn log_storage(mut self, log_storage: Arc<dyn LogStorage>) -> Self {
        self.log_storage = log_storage;
        self
    }

    pub fn email(mut self, email: Arc<dyn EmailSender>) -> Self {
        self.email = email;
        self
    }

    pub fn graph(mut self, graph: Arc<Graph>) -> Self {
        self.graph = Some(graph);
        self
    }

    pub fn started_at(mut self, at: DateTime<Utc>) -> Self {
        self.started_at = Some(at);
        self
    }

    pub fn store_path_mapping(mut self, mapping: Arc<dyn StorePathMapping>) -> Self {
        self.extensions.store_path_mapping = mapping;
        self
    }

    pub fn build(self) -> Arc<ServerState> {
        let mut config = RuntimeConfig::from_cli(&self.cli).expect("valid test config");
        for change in self.config_changes {
            change(&mut config);
        }
        let nar_storage = self.nar_storage.unwrap_or_else(|| {
            NarStore::local(&config.server.base_dir).expect("create test NarStore")
        });

        Arc::new(ServerState {
            web_db: WebDb::new(self.web_db),
            cache_db: CacheDb::new(self.cache_db),
            worker_db: WorkerDb::new(self.worker_db),
            config: Arc::new(config),
            log_storage: self.log_storage,
            email: self.email,
            nar_storage,
            http: gradient_util::http::build_client().expect("build test HTTP client"),
            git_host: gradient_git_host::GitHostRegistry::with_builtin(),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
            started_at: self.started_at.unwrap_or_else(Utc::now),
            events: gradient_types::EventBus::default(),
            wake: Default::default(),
            graph: self.graph.unwrap_or_else(Graph::stub),
            extensions: self.extensions,
            memory: InstanceMemory {
                db: Default::default(),
                last_used_stamps: gradient_core::last_used_stamps(),
                cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
                build_progress: gradient_core::build_progress(),
                eval_progress: gradient_core::eval_progress(),
                eval_member_thunks: gradient_core::eval_member_thunks(),
                build_status_counts: gradient_core::build_status_counts(),
                github_app_install_url: Default::default(),
                manifest_state: Default::default(),
                pending_credentials: Default::default(),
                pending_project_memberships: Default::default(),
                upstream_query: Arc::new(tokio::sync::Semaphore::new(32)),
                nar_downloads: Arc::new(tokio::sync::Semaphore::new(16)),
                upload_admission: gradient_storage::admission::UploadAdmission::new(
                    gradient_storage::admission::Limits {
                        concurrency: 16,
                        bytes: u64::MAX,
                    },
                ),
            },
        })
    }

    pub fn server(self) -> TestServer {
        TestServer::new(gradient_web::create_router(self.build()).expect("router"))
    }
}

pub fn test_state(worker_db: DatabaseConnection) -> Arc<ServerState> {
    TestState::new().worker_db(worker_db).build()
}
