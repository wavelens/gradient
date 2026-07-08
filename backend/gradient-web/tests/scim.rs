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
use gradient_entity::{organization_user, user};
use gradient_state::ScimGroupRoles;
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::{OrganizationId, RoleId, RuntimeConfig, UserId};
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
    build_server(db, hard_delete, ScimGroupRoles::new())
}

fn scim_server_with_groups(db: DatabaseConnection, groups: ScimGroupRoles) -> TestServer {
    build_server(db, false, groups)
}

fn build_server(
    db: DatabaseConnection,
    hard_delete: bool,
    scim_group_roles: ScimGroupRoles,
) -> TestServer {
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
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
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
        scim_group_roles: std::sync::Arc::new(scim_group_roles),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
    });

    TestServer::new(create_router(state).expect("router"))
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

fn group_with_grant(name: &str) -> (ScimGroupRoles, OrganizationId, RoleId) {
    let org = OrganizationId::now_v7();
    let role = RoleId::now_v7();
    let mut groups = ScimGroupRoles::new();
    groups.insert(name.to_string(), vec![(org, role)]);
    (groups, org, role)
}

fn membership(org: OrganizationId, user: UserId, role: RoleId) -> organization_user::Model {
    organization_user::Model {
        id: Uuid::now_v7().into(),
        organization: org,
        user,
        role,
    }
}

#[test]
fn scim_unknown_group_returns_404() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let s = scim_server(db); // empty scim_group_roles
        let res = s
            .get("/scim/v2/Groups/nope")
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn scim_get_group_lists_members() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (groups, org, role) = group_with_grant("acme-eng");
        let member = UserId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(org, member, role)]]) // members lookup
            .into_connection();
        let s = scim_server_with_groups(db, groups);
        let res = s
            .get("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["displayName"], "acme-eng");
        assert_eq!(body["members"][0]["value"], member.to_string());
    });
}

#[test]
fn scim_patch_group_add_member_inserts_membership() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (groups, org, role) = group_with_grant("acme-eng");
        let member = UserId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<organization_user::Model>::new()]) // existing? none
            .append_query_results([vec![membership(org, member, role)]]) // INSERT ... RETURNING
            .append_query_results([vec![membership(org, member, role)]]) // members lookup
            .into_connection();
        let s = scim_server_with_groups(db, groups);
        let res = s
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{"op": "add", "path": "members", "value": [{"value": member.to_string()}]}]
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["members"][0]["value"], member.to_string());
    });
}

#[test]
fn scim_patch_group_remove_member_deletes_membership() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (groups, _org, _role) = group_with_grant("acme-eng");
        let member = UserId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]) // DELETE
            .append_query_results([Vec::<organization_user::Model>::new()]) // members lookup: empty
            .into_connection();
        let s = scim_server_with_groups(db, groups);
        let res = s
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{"op": "remove", "path": format!("members[value eq \"{member}\"]")}]
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["members"].as_array().unwrap().len(), 0);
    });
}

#[test]
fn inactive_user_session_returns_403() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let user = scim_user("mallory@example.com", false);
        let now = chrono::Utc::now().naive_utc();
        let session = gradient_entity::session::Model {
            id: gradient_types::SessionId::now_v7(),
            user_id: user.id,
            created_at: now,
            expires_at: now + chrono::Duration::hours(24),
            last_used_at: now,
            revoked_at: None,
            user_agent: None,
            ip: None,
            remember_me: false,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![session.clone()]]) // decode_jwt session lookup
            .append_query_results([vec![session.clone()]]) // last_used_at UPDATE ... RETURNING
            .append_query_results([vec![user.clone()]]) // EUser::find_by_id -> inactive
            .into_connection();
        let s = scim_server(db);

        let claims = gradient_web::authorization::Cliams {
            iat: now.and_utc().timestamp() as usize,
            exp: (now + chrono::Duration::hours(24)).and_utc().timestamp() as usize,
            id: user.id,
            jti: session.id,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(b"test-jwt-secret"),
        )
        .expect("encode jwt");

        let res = s
            .get("/api/v1/user")
            .add_header("Authorization", format!("Bearer {token}"))
            .await;
        res.assert_status_forbidden();
    });
}

#[test]
fn scim_service_provider_config_ok() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let s = scim_server(db);
        let res = s
            .get("/scim/v2/ServiceProviderConfig")
            .add_header("Authorization", auth_header())
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["patch"]["supported"], true);
    });
}
