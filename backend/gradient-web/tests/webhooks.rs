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
use gradient_entity::{ids::*, project_user, role, webhook};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server_with, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
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

fn membership(role: RoleId) -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::new(Uuid::from_u128(0xaa)),
        project: project_id(),
        user: user_id(),
        role,
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

fn with_auth(session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn as_project_admin(session_id: SessionId) -> MockDatabase {
    let admin = gradient_types::consts::BASE_ROLE_ADMIN_ID;
    with_auth(session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![membership(admin)]])
        .append_query_results([vec![role_row(
            admin,
            gradient_db::permissions::admin_mask(),
        )]])
}

fn temp_crypt_secret_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    path.to_string_lossy().into_owned()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

#[test]
fn create_returns_the_secret_once() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let db = as_project_admin(session_id)
            .append_query_results([Vec::<webhook::Model>::new()])
            .append_query_results([vec![webhook_row()]]);
        let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));

        let res = server
            .post(PROJECT_URL)
            .add_header("authorization", bearer(session_id))
            .json(
                &json!({ "name": "ci", "url": "https://example.com/hook", "events": ["build.*"] }),
            )
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
    });
}

#[test]
fn read_webhook_never_returns_the_secret() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let db = as_project_admin(session_id).append_query_results([vec![webhook_row()]]);
        let server = make_test_server_with(db.into_connection(), None);

        let res = server
            .get(&format!("{PROJECT_URL}/{}", webhook_id()))
            .add_header("authorization", bearer(session_id))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"]["name"], "ci");
        assert!(body["message"].get("secret").is_none());
    });
}

#[test]
fn create_rejects_a_private_url() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server_with(as_project_admin(session_id).into_connection(), None);

        let res = server
            .post(PROJECT_URL)
            .add_header("authorization", bearer(session_id))
            .json(&json!({ "name": "ci", "url": "http://127.0.0.1/hook", "events": [] }))
            .await;

        res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    });
}

#[test]
fn a_view_role_cannot_manage_webhooks() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let view = gradient_types::consts::BASE_ROLE_VIEW_ID;
        let db = with_auth(session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![membership(view)]])
            .append_query_results([vec![role_row(view, gradient_db::permissions::view_mask())]]);
        let server = make_test_server_with(db.into_connection(), None);

        let res = server
            .get(PROJECT_URL)
            .add_header("authorization", bearer(session_id))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn instance_webhooks_require_a_superuser() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server_with(with_auth(session_id).into_connection(), None);

        let res = server
            .get("/api/v1/admin/webhooks")
            .add_header("authorization", bearer(session_id))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}
