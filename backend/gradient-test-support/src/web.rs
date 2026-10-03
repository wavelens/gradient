/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_core::ServerState;
use gradient_db::{CacheDb, WebDb, WorkerDb};
use gradient_entity::ids::{SessionId, UserId};
use gradient_entity::session;
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_types::{RuntimeConfig, SecretString};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use serde::Serialize;

use crate::cli::{test_cli, test_cli_with_crypt};
use crate::fakes::email::InMemoryEmailSender;
use crate::fixtures::user_id;
use crate::log_storage::NoopLogStorage;

/// Must equal the `jwt_secret` that `test_state` and `make_test_server` are installing.
pub const TEST_JWT_SECRET: &str = "test-jwt-secret";

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

pub fn make_token(session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(1)).timestamp() as usize,
        id: user_id(),
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

pub fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
}

pub fn make_test_server(db: DatabaseConnection) -> TestServer {
    make_test_server_with(db, None)
}

pub fn make_test_server_with(
    db: DatabaseConnection,
    crypt_secret_file: Option<String>,
) -> TestServer {
    let cli = match crypt_secret_file {
        Some(path) => test_cli_with_crypt(path),
        None => test_cli(),
    };
    server_from_cli(db, cli)
}

pub fn make_test_server_configured(
    db: DatabaseConnection,
    configure: impl FnOnce(&mut gradient_types::Cli),
) -> TestServer {
    let mut cli = test_cli();
    configure(&mut cli);
    server_from_cli(db, cli)
}

pub fn make_test_server_with_worker_db(
    db: DatabaseConnection,
    worker_db: DatabaseConnection,
) -> TestServer {
    server_with_pools(db, worker_db, test_cli())
}

fn server_from_cli(db: DatabaseConnection, cli: gradient_types::Cli) -> TestServer {
    server_with_pools(
        db,
        MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        cli,
    )
}

fn server_with_pools(
    db: DatabaseConnection,
    worker_db: DatabaseConnection,
    cli: gradient_types::Cli,
) -> TestServer {
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: CacheDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(worker_db),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
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
    });
    TestServer::new(gradient_web::create_router(state).expect("router"))
}
