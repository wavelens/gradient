/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! SCIM integration tests. The `/scim/v2/*` routes are mounted in a later task;
//! the middleware tests here only exercise the bearer-token guard.

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::user;
use gradient_storage::{EmailSender, NarStore};
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::RuntimeConfig;
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    scim_server_with(db, false)
}

fn scim_server_with(db: DatabaseConnection, hard_delete: bool) -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-scim-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_secret_file = jwt_path.to_string_lossy().into_owned();
    cli.scim.scim_enabled = true;
    cli.scim.scim_token_file = Some(write_token());
    cli.scim.scim_hard_delete = hard_delete;

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

fn auth_header() -> String {
    format!("Bearer {SCIM_TOKEN}")
}

fn scim_user(username: &str, active: bool) -> user::Model {
    user::Model {
        id: Uuid::now_v7().into(),
        username: username.to_string(),
        name: username.to_string(),
        email: username.to_string(),
        managed: true,
        email_verified: true,
        active,
        ..Default::default()
    }
}

/// One-row mock that satisfies sea-orm's `count()` parser (`COUNT(*) AS num_items`).
fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

#[test]
fn scim_create_user_returns_201() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let created = scim_user("alice@example.com", true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<user::Model>::new()]) // username-exists check: none
            .append_query_results([vec![created.clone()]]) // insert returns row
            .into_connection();
        let s = scim_server(db);
        let res = s
            .post("/scim/v2/Users")
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
                "userName": "alice@example.com",
                "emails": [{"value": "alice@example.com", "primary": true}],
                "active": true
            }))
            .await;

        res.assert_status(StatusCode::CREATED);
        let body: Value = res.json();
        assert_eq!(body["userName"], "alice@example.com");
        assert_eq!(body["id"], created.id.to_string());
        assert_eq!(body["active"], true);
    });
}

#[test]
fn scim_get_user_returns_resource() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let u = scim_user("bob@example.com", true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![u.clone()]])
            .into_connection();
        let s = scim_server(db);
        let res = s
            .get(&format!("/scim/v2/Users/{}", u.id))
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["userName"], "bob@example.com");
        assert_eq!(body["meta"]["resourceType"], "User");
    });
}

#[test]
fn scim_list_users_with_username_filter() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let u = scim_user("carol@example.com", true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(1)]]) // COUNT(*)
            .append_query_results([vec![u.clone()]]) // page rows
            .into_connection();
        let s = scim_server(db);
        let res = s
            .get("/scim/v2/Users")
            .add_query_param("filter", r#"userName eq "carol@example.com""#)
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["totalResults"], 1);
        assert_eq!(body["Resources"][0]["userName"], "carol@example.com");
    });
}

#[test]
fn scim_patch_user_active_false() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let u = scim_user("dave@example.com", true);
        let disabled = user::Model {
            active: false,
            ..u.clone()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![u.clone()]]) // find_user
            .append_query_results([vec![disabled]]) // UPDATE ... RETURNING
            .into_connection();
        let s = scim_server(db);
        let res = s
            .patch(&format!("/scim/v2/Users/{}", u.id))
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{"op": "replace", "path": "active", "value": false}]
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["active"], false);
    });
}

#[test]
fn scim_delete_user_soft_disables() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let u = scim_user("erin@example.com", true);
        // Soft delete issues an UPDATE (RETURNING) and never a DELETE exec; staging
        // only a query result means a stray DELETE would fail with no exec staged.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![u.clone()]]) // find_user
            .append_query_results([vec![user::Model {
                active: false,
                ..u.clone()
            }]]) // UPDATE ... RETURNING
            .into_connection();
        let s = scim_server(db);
        let res = s
            .delete(&format!("/scim/v2/Users/{}", u.id))
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status(StatusCode::NO_CONTENT);
    });
}

#[test]
fn scim_delete_user_hard_deletes() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let u = scim_user("frank@example.com", true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![u.clone()]]) // find_user
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]) // DELETE
            .into_connection();
        let s = scim_server_with(db, true);
        let res = s
            .delete(&format!("/scim/v2/Users/{}", u.id))
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status(StatusCode::NO_CONTENT);
    });
}
