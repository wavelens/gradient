/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! SCIM integration tests. The `/scim/v2/*` routes are mounted in a later task;
//! the middleware tests here only exercise the bearer-token guard.

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_storage::{EmailSender, NarStore};
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::RuntimeConfig;
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const SCIM_TOKEN: &str = "test-scim-token";

fn write_token() -> String {
    let path = std::env::temp_dir().join(format!("gradient-scim-token-{}", Uuid::now_v7()));
    std::fs::write(&path, SCIM_TOKEN).expect("write scim token file");
    path.to_string_lossy().into_owned()
}

/// Build a `TestServer` with SCIM enabled against the given mock DB. Mirrors the
/// `ServerState` field set from `tests/auth_middleware.rs`; the only differences
/// are the SCIM config on the `Cli` and a writable jwt/scim secret file on disk.
fn scim_server(db: DatabaseConnection) -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-scim-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_secret_file = jwt_path.to_string_lossy().into_owned();
    cli.scim.scim_enabled = true;
    cli.scim.scim_token_file = Some(write_token());

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
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
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
    });

    TestServer::new(create_router(state))
}

#[test]
fn scim_missing_token_returns_401() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let s = scim_server(db);
        let res = s.get("/scim/v2/Users").await;
        res.assert_status_unauthorized();
        let body: Value = res.json();
        assert_eq!(
            body["schemas"][0],
            "urn:ietf:params:scim:api:messages:2.0:Error"
        );
    });
}

#[test]
fn scim_wrong_token_returns_401() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let s = scim_server(db);
        let res = s
            .get("/scim/v2/Users")
            .add_header("Authorization", "Bearer nope")
            .await;
        res.assert_status_unauthorized();
    });
}
