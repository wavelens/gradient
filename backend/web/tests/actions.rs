/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for project_action CRUD endpoints.
//!
//! Same pattern as `triggers.rs`: manual Tokio runtime + `axum_test::TestServer`
//! + `MockDatabase`. The SMTP-disabled test builds its own `ServerState` so it
//!   can swap in an `InMemoryEmailSender::disabled()`.

use axum_test::TestServer;
use entity::{ids::*, organization_user, project, project_action, project_action_delivery};
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, SessionId, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};
use std::sync::Arc;
use test_support::cli::{test_cli, test_cli_with_crypt};
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::log_storage::NoopLogStorage;
use test_support::web::{TEST_JWT_SECRET, live_session, make_test_server_with, make_token};
use uuid::Uuid;
use web::create_router;

// ── Fixture helpers ────────────────────────────────────────────────────────────

fn action_id() -> ProjectActionId {
    ProjectActionId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap())
}

fn project_row() -> project::Model {
    project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
    }
}

fn admin_membership() -> organization_user::Model {
    organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn admin_role_row() -> entity::role::Model {
    entity::role::Model {
        id: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        organization: None,
        permission: gradient_core::permissions::admin_mask(),
        managed: false,
    }
}

fn send_mail_action_row() -> project_action::Model {
    project_action::Model {
        id: action_id(),
        project: project_id(),
        name: "ops-mail".into(),
        action_type: 0,
        config: json!({
            "type": "send_mail",
            "recipients": ["ops@example.com"],
        }),
        events: json!(["build.completed"]),
        active: true,
        last_fired_at: None,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn web_request_action_row() -> project_action::Model {
    project_action::Model {
        id: action_id(),
        project: project_id(),
        name: "hook".into(),
        action_type: 1,
        config: json!({
            "type": "send_web_request",
            "url": "https://example.com/hook",
            "token": "ENCRYPTED_BLOB",
        }),
        events: json!(["build.completed"]),
        active: true,
        last_fired_at: None,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
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

fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

fn with_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

/// Builds a `TestServer` with a custom email-sender. Used for the
/// SMTP-disabled gating test; other tests use `make_test_server_with`.
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
    let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
    });
    TestServer::new(create_router(state))
}

const BASE_URL: &str = "/api/v1/projects/test-org/test-project/actions";

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
fn list_actions_empty() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_action::Model>::new()]);

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
    });
}

#[test]
fn create_send_mail_returns_201_when_smtp_enabled() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_action::Model>::new()])
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
    });
}

#[test]
fn create_send_mail_422_when_smtp_disabled() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
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
    });
}

#[test]
fn create_send_web_request_returns_token_once() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_action::Model>::new()])
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
    });
}

#[test]
fn create_forge_status_report_rejects_nonempty_events() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
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
                    "type": "forge_status_report",
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
                .contains("forge_status_report"),
            "expected forge_status_report mention, got: {}",
            body["message"]
        );
    });
}

#[test]
fn read_action_strips_token_from_config() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
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
    });
}

#[test]
fn update_rejects_action_type_change() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // Existing row is send_mail (action_type = 0); PATCH with send_web_request config.
        let db = with_project_edit(with_auth(
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
    });
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

#[test]
fn regenerate_token_returns_new_plaintext() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
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
    });
}

#[test]
fn regenerate_token_rejects_non_web_request_action() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
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
    });
}

fn delivery_id() -> ProjectActionDeliveryId {
    ProjectActionDeliveryId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000d1").unwrap())
}

fn delivery_row() -> project_action_delivery::Model {
    project_action_delivery::Model {
        id: delivery_id(),
        action_id: action_id(),
        event: "build.completed".into(),
        request_body: r#"{"event":"build.completed"}"#.into(),
        response_status: Some(200),
        response_body: Some(r#"{"ok":true}"#.into()),
        error_message: None,
        success: true,
        duration_ms: 42,
        delivered_at: test_date(),
    }
}

#[test]
fn list_deliveries_excludes_bodies() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
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
    });
}

#[test]
fn get_delivery_detail_includes_bodies() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
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
    });
}

#[test]
fn list_deliveries_404_on_unknown_action() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_action::Model>::new()]);

        let server = make_test_server_with(db.into_connection(), None);
        let unknown_id = ProjectActionId::now_v7();
        let url = format!("{}/{}/deliveries", BASE_URL, unknown_id);
        let res = server
            .get(&url)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::NOT_FOUND);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn delete_returns_404_when_unknown() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_action::Model>::new()]);

        let server = make_test_server_with(db.into_connection(), None);
        let unknown_id = ProjectActionId::now_v7();
        let url = format!("{}/{}", BASE_URL, unknown_id);
        let res = server
            .delete(&url)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::NOT_FOUND);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}
