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
use gradient_entity::{ids::*, task_action, task_action_delivery, team};
use gradient_fixtures::ids::{task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_fixtures::test_date;
use gradient_notify::EmailSender;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::state::TestState;
use gradient_test_support::web::make_token;
use gradient_test_support::web::{make_test_server, with_auth};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::{ActionType, SessionId};
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn action_id() -> TaskActionId {
    TaskActionId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap())
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

fn with_task_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
}

fn with_task_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

fn server_with_email(
    db: sea_orm::DatabaseConnection,
    email: Arc<dyn EmailSender>,
    crypt_secret_file: Option<String>,
) -> TestServer {
    TestState::new()
        .web_db(db)
        .email(email)
        .cli(|cli| {
            if let Some(path) = crypt_secret_file {
                cli.secrets.crypt_file = path;
            }
        })
        .server()
}

const BASE_URL: &str = "/api/v1/tasks/test-project/test-task/actions";

#[tokio::test]
async fn list_actions_empty() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server(db.into_connection());
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

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server(db.into_connection());
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
async fn a_mail_action_cannot_address_a_team_without_access() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<team::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({
            "name": "team-mail",
            "config": {
                "type": "send_mail",
                "recipients": ["team:ghosts"],
            },
            "events": ["build.completed"],
        }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_send_mail_422_when_smtp_disabled() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

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

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![web_request_action_row()]]);

    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();
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

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![matrix_action_row()]]);

    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();
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

async fn create_rejected(config: Value) -> Value {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&json!({ "name": "chat", "config": config, "events": ["build.failed"] }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    res.json()
}

#[tokio::test]
async fn create_chat_action_invalid_config_returns_422() {
    let cases: &[(Value, &str)] = &[
        (
            json!({
                "type": "send_matrix_message",
                "homeserver": "https://matrix.example.org",
                "room_id": "!ops:example.org",
            }),
            "access_token",
        ),
        (
            json!({
                "type": "send_matrix_message",
                "homeserver": "https://matrix.example.org",
                "room_id": "!ops:example.org",
                "access_token": " ",
            }),
            "access_token",
        ),
        (
            json!({
                "type": "send_matrix_message",
                "homeserver": "https://matrix.example.org",
                "room_id": "#ops:example.org",
                "access_token": "t",
            }),
            "room_id",
        ),
        (
            json!({
                "type": "send_slack_message",
                "webhook_url": "http://127.0.0.1/hook",
            }),
            "disallowed address",
        ),
    ];

    for (config, expected) in cases {
        let body = create_rejected(config.clone()).await;
        assert!(
            body["message"].as_str().unwrap().contains(expected),
            "{config}: {body}"
        );
    }
}

#[tokio::test]
async fn create_git_host_status_report_rejects_nonempty_events() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
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
async fn read_action_secret_config_field_is_stripped() {
    let slack_row = task_action::Model {
        action_type: ActionType::SendSlackMessage,
        config: json!({ "type": "send_slack_message", "webhook_url": "ENCRYPTED_BLOB" }),
        ..matrix_action_row()
    };
    let cases = [
        (web_request_action_row(), "send_web_request", "token"),
        (slack_row, "send_slack_message", "webhook_url"),
    ];

    for (row, action_type, secret_field) in cases {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db =
            with_task_member(with_auth(mock_db(), session_id)).append_query_results([vec![row]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, action_id()))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["action_type"], action_type);
        assert!(
            body["message"]["config"].get(secret_field).is_none(),
            "{action_type}: {}",
            body["message"]["config"]
        );
    }
}

#[tokio::test]
async fn update_rejects_action_type_change() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server(db.into_connection());
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

#[tokio::test]
async fn regenerate_token_returns_new_plaintext() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![web_request_action_row()]])
        .append_query_results([vec![web_request_action_row()]]);

    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();
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

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![send_mail_action_row()]]);

    let server = make_test_server(db.into_connection());
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

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![send_mail_action_row()]])
        .append_query_results([vec![delivery_row()]]);

    let server = make_test_server(db.into_connection());
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

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![send_mail_action_row()]])
        .append_query_results([vec![delivery_row()]]);

    let server = make_test_server(db.into_connection());
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

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server(db.into_connection());
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

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_action::Model>::new()]);

    let server = make_test_server(db.into_connection());
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
