/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::team_user::TeamRole;
use gradient_entity::{team, team_user, user};
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::{RuntimeConfig, TeamId, TeamUserId, UserId};
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

fn scim_server(db: DatabaseConnection) -> TestServer {
    scim_server_with(db, false)
}

fn scim_server_with(db: DatabaseConnection, hard_delete: bool) -> TestServer {
    build_server(db, hard_delete)
}

fn build_server(db: DatabaseConnection, hard_delete: bool) -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-scim-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let mut cli = test_cli();
    cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned();
    cli.scim.enable = true;
    cli.scim.token_file = Some(write_token());
    cli.scim.hard_delete = hard_delete;

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("create test NarStore");
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
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    });

    TestServer::new(create_router(state).expect("router"))
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_missing_token_returns_401() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let s = scim_server(db);
    let res = s.get("/scim/v2/Users").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    assert_eq!(
        body["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_wrong_token_returns_401() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let s = scim_server(db);
    let res = s
        .get("/scim/v2/Users")
        .add_header("Authorization", "Bearer nope")
        .await;
    res.assert_status_unauthorized();
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

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_create_user_returns_201() {
    let created = scim_user("alice@example.com", true);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<user::Model>::new()])
        .append_query_results([vec![created.clone()]])
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
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_get_user_returns_resource() {
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
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_list_users_with_username_filter() {
    let u = scim_user("carol@example.com", true);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![count_row(1)]])
        .append_query_results([vec![u.clone()]])
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
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_user_active_false() {
    let u = scim_user("dave@example.com", true);
    let disabled = user::Model {
        active: false,
        ..u.clone()
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![u.clone()]])
        .append_query_results([vec![disabled]])
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
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_delete_user_soft_disables() {
    let u = scim_user("erin@example.com", true);
    // Soft delete is issuing an UPDATE (RETURNING) and never a DELETE exec. Only a query result
    // is staged, and a stray DELETE would fail with no exec staged.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![u.clone()]])
        .append_query_results([vec![user::Model {
            active: false,
            ..u.clone()
        }]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .delete(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_delete_user_hard_deletes() {
    let u = scim_user("frank@example.com", true);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![u.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let s = scim_server_with(db, true);
    let res = s
        .delete(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status(StatusCode::NO_CONTENT);
}

fn scim_team() -> team::Model {
    team::Model {
        id: TeamId::now_v7(),
        name: "acme-eng".into(),
        display_name: "ACME Eng".into(),
        scim_group: Some("acme-eng".into()),
        ..Default::default()
    }
}

fn team_member(team: TeamId, user: UserId) -> team_user::Model {
    team_user::Model {
        id: TeamUserId::now_v7(),
        team,
        user,
        role: TeamRole::Member,
        via_group: true,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_unknown_group_returns_404() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<team::Model>::new()])
        .into_connection();
    let res = scim_server(db)
        .get("/scim/v2/Groups/nope")
        .add_header("Authorization", auth_header())
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_get_group_lists_the_team_members() {
    let team = scim_team();
    let member = UserId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![team.clone()]])
        .append_query_results([vec![team_member(team.id, member)]])
        .into_connection();
    let res = scim_server(db)
        .get("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["members"][0]["value"], member.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_group_add_member_joins_the_team() {
    let team = scim_team();
    let member = UserId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![team.clone()]])
        .append_query_results([Vec::<team_user::Model>::new()])
        .append_query_results([vec![team_member(team.id, member)]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![team_member(team.id, member)]])
        .into_connection();
    let res = scim_server(db)
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
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_group_remove_member_leaves_the_team() {
    let team = scim_team();
    let member = UserId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![team.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<team_user::Model>::new()])
        .into_connection();
    let server = scim_server(db.clone());
    let res = server
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "remove", "path": format!("members[value eq \"{member}\"]")}]
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["members"].as_array().map(Vec::len), Some(0));
    drop(server);
    let only_group_rows = db
        .into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .any(|s| s.sql.starts_with("DELETE FROM \"team_user\"") && s.sql.contains("\"via_group\""));
    assert!(only_group_rows, "SCIM must not remove members added by hand");
}

#[tokio::test(flavor = "multi_thread")]
async fn inactive_user_session_returns_403() {
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
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![user.clone()]])
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
}
