/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::webhook::WebhookScope;
use gradient_entity::{ids::*, role, webhook};
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{project, project_member};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, make_test_server};
use gradient_test_support::web::{signed_in, with_auth};
use gradient_types::SessionId;
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use uuid::Uuid;

const PROJECT_URL: &str = "/api/v1/projects/test-project/webhooks";

fn webhook_id() -> WebhookId {
    WebhookId::new(Uuid::from_u128(0xb1))
}

fn webhook_row() -> webhook::Model {
    webhook::Model {
        id: webhook_id(),
        scope: WebhookScope::Project,
        project: Some(project_id()),
        cache: None,
        name: "ci".into(),
        url: "https://example.com/hook".into(),
        secret: "ENCRYPTED".into(),
        events: json!(["build.*"]),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        last_fired_at: None,
    }
}

fn role_row(id: RoleId, permission: i64) -> role::Model {
    role::Model {
        id,
        name: "r".into(),
        permission,
        ..Default::default()
    }
}

fn as_project_admin(session_id: SessionId) -> MockDatabase {
    let admin = gradient_types::consts::BASE_ROLE_ADMIN_ID;
    with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(admin)]])
        .append_query_results([vec![role_row(
            admin,
            gradient_types::permissions::admin_mask(),
        )]])
}

fn temp_crypt_secret_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn create_returns_the_secret_once() {
    let session_id = SessionId::now_v7();
    let db = as_project_admin(session_id)
        .append_query_results([Vec::<webhook::Model>::new()])
        .append_query_results([vec![webhook_row()]]);
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();

    let res = server
        .post(PROJECT_URL)
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "ci", "url": "https://example.com/hook", "events": ["build.*"] }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert!(
        body["message"]["secret"]
            .as_str()
            .unwrap()
            .starts_with("whs_")
    );
    assert!(body["message"]["webhook"].get("secret").is_none());
    assert_eq!(body["message"]["webhook"]["scope"], "project");
}

#[tokio::test]
async fn read_webhook_never_returns_the_secret() {
    let session_id = SessionId::now_v7();
    let db = as_project_admin(session_id).append_query_results([vec![webhook_row()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("{PROJECT_URL}/{}", webhook_id()))
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["name"], "ci");
    assert!(body["message"].get("secret").is_none());
}

#[tokio::test]
async fn create_rejects_a_private_url() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(as_project_admin(session_id).into_connection());

    let res = server
        .post(PROJECT_URL)
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "ci", "url": "http://127.0.0.1/hook", "events": [] }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_rejects_an_unbounded_event_pattern() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(as_project_admin(session_id).into_connection());

    let res = server
        .post(PROJECT_URL)
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "ci", "url": "https://example.com/hook", "events": ["a**b"] }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_view_role_cannot_manage_webhooks() {
    let (db, auth) = signed_in();
    let view = gradient_types::consts::BASE_ROLE_VIEW_ID;
    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(view)]])
        .append_query_results([vec![role_row(
            view,
            gradient_types::permissions::view_mask(),
        )]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(PROJECT_URL)
        .add_header("authorization", auth)
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn instance_webhooks_require_a_superuser() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(with_auth(mock_db(), session_id).into_connection());

    let res = server
        .get("/api/v1/admin/webhooks")
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}
