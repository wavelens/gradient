/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::{ids::*, project_user, task, task_action, task_action_delivery};
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::{test_cli, test_cli_with_crypt};
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{project, project_id, task_id, test_date, user, user_id};
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::web::{
    TEST_JWT_SECRET, live_session, make_test_server_with, make_token,
};
use gradient_types::{ActionType, ConcurrencyPolicy, RuntimeConfig, SecretString, SessionId};
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn action_id() -> TaskActionId {
    TaskActionId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap())
}

fn task_row() -> task::Model {
    task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::Skip,
        sign_cache: true,
        ..Default::default()
    }
}

fn admin_membership() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn admin_role_row() -> gradient_entity::role::Model {
    gradient_entity::role::Model {
        id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_db::permissions::admin_mask(),
        ..Default::default()
    }
}

fn send_mail_action_row() -> task_action::Model {
    task_action::Model {
        id: action_id(),
        task: task_id(),
        name: "ops-mail".into(),
        config: json!({
            "type": "send_mail",
            "recipients": ["ops@example.com"],
        }),
        events: json!(["build.completed"]),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn web_request_action_row() -> task_action::Model {
    task_action::Model {
        id: action_id(),
        task: task_id(),
        name: "hook".into(),
        action_type: ActionType::SendWebRequest,
        config: json!({
            "type": "send_web_request",
            "url": "https://example.com/hook",
            "token": "ENCRYPTED_BLOB",
        }),
        events: json!(["build.completed"]),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn temp_crypt_secret_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    path.to_string_lossy().into_owned()
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn with_task_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![admin_membership()]])
}

fn with_task_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

fn server_with_email(
    db: sea_orm::DatabaseConnection,
    email: Arc<dyn EmailSender>,
    crypt_secret_file: Option<String>,
) -> TestServer {
    let cli = match crypt_secret_file {
        Some(path) => test_cli_with_crypt(path),
        None => test_cli(),
    };
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
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
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    });
    TestServer::new(create_router(state).expect("router"))
}

const BASE_URL: &str = "/api/v1/tasks/test-project/test-task/actions";

#[tokio::test]
async fn list_actions_empty() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server_with(db.into_connection(), None);
    let res = server
        .get(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let items = body["message"].as_array().expect("array");
    assert!(items.is_empty());
}

#[tokio::test]
async fn create_send_mail_returns_201_when_smtp_enabled() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()])
    .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "ops-mail",
            "config": {
                "type": "send_mail",
                "recipients": ["ops@example.com"],
            },
            "events": ["build.completed"],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["action"]["action_type"], "send_mail");
    assert_eq!(body["message"]["action"]["name"], "ops-mail");
    assert!(body["message"]["token"].is_null());
}

#[tokio::test]
async fn create_send_mail_422_when_smtp_disabled() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ));

    let server = server_with_email(
        db.into_connection(),
        Arc::new(InMemoryEmailSender::disabled()) as Arc<dyn EmailSender>,
        None,
    );
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "ops-mail",
            "config": {
                "type": "send_mail",
                "recipients": ["ops@example.com"],
            },
        }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("SMTP"),
        "expected SMTP mention, got: {}",
        body["message"]
    );
}

#[tokio::test]
async fn create_send_web_request_returns_token_once() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()])
    .append_query_results([vec![web_request_action_row()]]);

    let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "hook",
            "config": {
                "type": "send_web_request",
                "url": "https://example.com/hook",
                "token": "supersecret",
            },
            "events": ["build.completed"],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["action"]["action_type"], "send_web_request");
    assert_eq!(body["message"]["token"], "supersecret");
    assert!(
        body["message"]["action"]["config"].get("token").is_none(),
        "stored config must not echo the token back: {}",
        body["message"]["action"]["config"]
    );
}

fn matrix_action_row() -> task_action::Model {
    task_action::Model {
        id: action_id(),
        task: task_id(),
        name: "matrix".into(),
        action_type: ActionType::SendMatrixMessage,
        config: json!({
            "type": "send_matrix_message",
            "homeserver": "https://matrix.example.org",
            "room_id": "!ops:example.org",
            "access_token": "ENCRYPTED_BLOB",
        }),
        events: json!(["build.failed"]),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_matrix_action_never_echoes_the_access_token() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()])
    .append_query_results([vec![matrix_action_row()]]);

    let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "matrix",
            "config": {
                "type": "send_matrix_message",
                "homeserver": "https://matrix.example.org",
                "room_id": "!ops:example.org",
                "access_token": "syt_plain",
            },
            "events": ["build.failed"],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"]["action"]["action_type"],
        "send_matrix_message"
    );
    assert!(body["message"].get("token").is_none());
    assert!(
        body["message"]["action"]["config"]
            .get("access_token")
            .is_none()
    );
}

#[tokio::test]
async fn read_slack_action_strips_the_webhook_url() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let row = task_action::Model {
        action_type: ActionType::SendSlackMessage,
        config: json!({ "type": "send_slack_message", "webhook_url": "ENCRYPTED_BLOB" }),
        ..matrix_action_row()
    };
    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![row]]);

    let server = make_test_server_with(db.into_connection(), None);
    let res = server
        .get(&format!("{}/{}", BASE_URL, action_id()))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["action_type"], "send_slack_message");
    assert!(body["message"]["config"].get("webhook_url").is_none());
}

async fn create_rejected(config: Value) -> Value {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ));

    let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({ "name": "chat", "config": config, "events": ["build.failed"] }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    res.json()
}

#[tokio::test]
async fn create_matrix_action_requires_an_access_token() {
    let body = create_rejected(json!({
        "type": "send_matrix_message",
        "homeserver": "https://matrix.example.org",
        "room_id": "!ops:example.org",
    }))
    .await;
    assert!(
        body["message"].as_str().unwrap().contains("access_token"),
        "{body}"
    );
}

#[tokio::test]
async fn create_matrix_action_rejects_an_empty_access_token() {
    let body = create_rejected(json!({
        "type": "send_matrix_message",
        "homeserver": "https://matrix.example.org",
        "room_id": "!ops:example.org",
        "access_token": " ",
    }))
    .await;
    assert!(
        body["message"].as_str().unwrap().contains("access_token"),
        "{body}"
    );
}

#[tokio::test]
async fn create_matrix_action_rejects_a_room_alias() {
    let body = create_rejected(json!({
        "type": "send_matrix_message",
        "homeserver": "https://matrix.example.org",
        "room_id": "#ops:example.org",
        "access_token": "t",
    }))
    .await;
    assert!(
        body["message"].as_str().unwrap().contains("room_id"),
        "{body}"
    );
}

#[tokio::test]
async fn create_slack_action_rejects_a_loopback_webhook() {
    let body = create_rejected(json!({
        "type": "send_slack_message",
        "webhook_url": "http://127.0.0.1/hook",
    }))
    .await;
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("disallowed address"),
        "{body}"
    );
}

#[tokio::test]
async fn create_git_host_status_report_rejects_nonempty_events() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ));

    let server = make_test_server_with(db.into_connection(), None);
    let integration_id = IntegrationId::now_v7();
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "status",
            "config": {
                "type": "git_host_status_report",
                "integration_id": integration_id.to_string(),
            },
            "events": ["build.started"],
        }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("git_host_status_report"),
        "expected git_host_status_report mention, got: {}",
        body["message"]
    );
}

#[tokio::test]
async fn read_action_strips_token_from_config() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![web_request_action_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let url = format!("{}/{}", BASE_URL, action_id());
    let res = server
        .get(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert!(
        body["message"]["config"].get("token").is_none(),
        "token must be stripped from read response: {}",
        body["message"]["config"]
    );
    assert_eq!(body["message"]["action_type"], "send_web_request");
}

#[tokio::test]
async fn update_rejects_action_type_change() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let url = format!("{}/{}", BASE_URL, action_id());
    let res = server
        .patch(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "config": {
                "type": "send_web_request",
                "url": "https://example.com/hook",
            },
        }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("action_type"),
        "expected action_type mention, got: {}",
        body["message"]
    );
}

#[test]
#[ignore = "needs end-to-end harness: MockDatabase prescribes query expectations, making it impractical to verify encrypted token preservation via raw row read after update"]
fn update_send_web_request_without_token_preserves_existing() {}

#[test]
#[ignore = "test-fire calls execute_action which writes a delivery row via state.worker_db; \
            the worker_db MockDatabase in make_test_server_with has no prescripted rows for the \
            worker-side INSERT + UPDATE, causing MockDatabase to return an error. \
            A real integration test with a live DB is needed to validate end-to-end."]
fn test_fire_returns_ok_for_send_web_request() {}

#[tokio::test]
async fn regenerate_token_returns_new_plaintext() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![web_request_action_row()]])
    .append_query_results([vec![web_request_action_row()]]);

    let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));
    let url = format!("{}/{}/regenerate-token", BASE_URL, action_id());
    let res = server
        .post(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let new_token = body["message"]["token"].as_str().expect("token string");
    assert!(new_token.starts_with("gat_"), "token prefix: {}", new_token);
    assert_ne!(new_token, "old", "token must be newly generated");
}

#[tokio::test]
async fn regenerate_token_rejects_non_web_request_action() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let url = format!("{}/{}/regenerate-token", BASE_URL, action_id());
    let res = server
        .post(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("send_web_request"),
        "expected send_web_request mention, got: {}",
        body["message"]
    );
}

fn delivery_id() -> TaskActionDeliveryId {
    TaskActionDeliveryId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000d1").unwrap())
}

fn delivery_row() -> task_action_delivery::Model {
    task_action_delivery::Model {
        id: delivery_id(),
        action_id: action_id(),
        event: "build.completed".into(),
        request_body: r#"{"event":"build.completed"}"#.into(),
        response_status: Some(200),
        response_body: Some(r#"{"ok":true}"#.into()),
        success: true,
        duration_ms: 42,
        delivered_at: test_date(),
        ..Default::default()
    }
}

#[tokio::test]
async fn list_deliveries_excludes_bodies() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![send_mail_action_row()]])
    .append_query_results([vec![delivery_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let url = format!("{}/{}/deliveries", BASE_URL, action_id());
    let res = server
        .get(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let items = body["message"].as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert!(
        items[0].get("request_body").is_none(),
        "list must not expose request_body"
    );
    assert!(
        items[0].get("response_body").is_none(),
        "list must not expose response_body"
    );
    assert_eq!(items[0]["event"], "build.completed");
    assert_eq!(items[0]["success"], true);
}

#[tokio::test]
async fn get_delivery_detail_includes_bodies() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([vec![send_mail_action_row()]])
    .append_query_results([vec![delivery_row()]]);

    let server = make_test_server_with(db.into_connection(), None);
    let url = format!("{}/{}/deliveries/{}", BASE_URL, action_id(), delivery_id());
    let res = server
        .get(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let msg = &body["message"];
    assert_eq!(msg["request_body"], r#"{"event":"build.completed"}"#);
    assert_eq!(msg["response_body"], r#"{"ok":true}"#);
    assert_eq!(msg["event"], "build.completed");
}

#[tokio::test]
async fn list_deliveries_404_on_unknown_action() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server_with(db.into_connection(), None);
    let unknown_id = TaskActionId::now_v7();
    let url = format!("{}/{}/deliveries", BASE_URL, unknown_id);
    let res = server
        .get(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn delete_returns_404_when_unknown() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
    .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server_with(db.into_connection(), None);
    let unknown_id = TaskActionId::now_v7();
    let url = format!("{}/{}", BASE_URL, unknown_id);
    let res = server
        .delete(&url)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}
