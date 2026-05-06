/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for project_trigger CRUD endpoints.
//!
//! Pattern: manual Tokio runtime + `axum_test::TestServer` + `MockDatabase`.
//! Uses manual runtimes because `#[tokio::test]` expands to `::gradient_core::…`
//! which clashes with the local `core` crate name in this workspace.
//!
//! Auth sequence per request through `authorize` middleware (in order):
//!   1. SELECT session (by jti)
//!   2. UPDATE session (last_used_at)
//!   3. SELECT user (by id)
//!
//! Then per `load_project`:
//!   4. SELECT org (by name)
//!   5. SELECT project (by org + name)
//!   6. SELECT org_user membership (permission check)

use axum_test::TestServer;
use chrono::{Duration, Utc};
use entity::{ids::*, organization_user, project, project_trigger, session};
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, SessionId, WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use test_support::cli::test_cli;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use web::create_router;

const JWT_SECRET: &str = "test-jwt-secret";

// ── Auth helpers ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

fn make_token(session_id: SessionId) -> String {
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
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(1),
        last_used_at: now,
        revoked_at: None,
        user_agent: None,
        ip: None,
        remember_me: false,
    }
}

// ── Server factory ─────────────────────────────────────────────────────────────

fn make_server(db: sea_orm::DatabaseConnection) -> TestServer {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn gradient_core::ci::WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
    });
    TestServer::new(create_router(state))
}

// ── Fixture helpers ────────────────────────────────────────────────────────────

fn trigger_id() -> ProjectTriggerId {
    ProjectTriggerId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap())
}

fn project_row() -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: "".into(),
        repository: "https://github.com/test/repo".into(),
        evaluation_wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 10,
    }
}

fn admin_membership() -> organization_user::Model {
    organization_user::Model {
        id: OrganizationUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn polling_trigger_row() -> project_trigger::Model {
    project_trigger::Model {
        id: trigger_id(),
        project: project_id(),
        trigger_type: 0, // Polling
        concurrency: 3,  // Skip
        config: serde_json::json!({"interval_secs": 60}),
        active: true,
        last_fired_at: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

/// Append the standard auth mock sequence:
/// 1. SELECT session (decode_jwt validates session)
/// 2. SELECT session (UPDATE ... RETURNING — Postgres backend uses RETURNING path)
/// 3. SELECT user
fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

/// Append a `load_project` sequence with Member access (no permission row needed):
/// 1. SELECT org
/// 2. SELECT project
/// 3. SELECT org_user (membership check)
fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

/// Append a `load_project` sequence with Require(EditProject) access:
/// 1. SELECT org
/// 2. SELECT project
/// 3. SELECT org_user (permission check)
fn with_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

const BASE_URL: &str = "/api/v1/projects/test-org/test-project/triggers";

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
fn list_triggers_returns_rows() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]]);

        let server = make_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        let items = body["message"].as_array().expect("message is array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "polling");
        assert_eq!(items[0]["concurrency"], "skip");
        assert_eq!(items[0]["active"], true);
    });
}

#[test]
fn get_trigger_returns_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_member(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]]);

        let server = make_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "polling");
        assert_eq!(body["message"]["id"], tid.to_string());
    });
}

#[test]
fn get_trigger_not_found_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = ProjectTriggerId::now_v7();

        let db = with_project_member(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([Vec::<project_trigger::Model>::new()]);

        let server = make_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn create_polling_trigger_valid() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]]);

        let server = make_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "polling", "interval_secs": 60},
                "concurrency": "skip"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "polling");
        assert_eq!(body["message"]["concurrency"], "skip");
    });
}

#[test]
fn create_polling_trigger_interval_too_small_returns_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id));

        let server = make_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "polling", "interval_secs": 5},
                "concurrency": "skip"
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        let msg = body["message"].as_str().unwrap();
        assert!(msg.contains("interval_secs"), "expected interval message, got: {msg}");
    });
}

#[test]
fn create_concurrency_allow_returns_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id));

        let server = make_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "polling", "interval_secs": 60},
                "concurrency": "allow"
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(
            body["message"].as_str().unwrap().contains("allow"),
            "expected allow mention, got: {}",
            body["message"]
        );
    });
}

#[test]
fn create_invalid_cron_returns_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id));

        let server = make_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "time", "cron": "not a cron"},
                "concurrency": "skip"
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn patch_trigger_updates_fields() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let updated = project_trigger::Model {
            concurrency: 0, // HardAbort
            ..polling_trigger_row()
        };

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]])
            .append_query_results([vec![updated]]);

        let server = make_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"concurrency": "hard_abort"}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["concurrency"], "hard_abort");
    });
}

#[test]
fn patch_trigger_config_type_change() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let updated = project_trigger::Model {
            trigger_type: 3, // Time
            concurrency: 3,
            config: serde_json::json!({"cron": "0 0 2 * * *"}),
            ..polling_trigger_row()
        };

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]])
            .append_query_results([vec![updated]]);

        let server = make_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "time", "cron": "0 0 2 * * *"}
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "time");
    });
}

#[test]
fn patch_trigger_allow_concurrency_returns_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]]);

        let server = make_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"concurrency": "allow"}))
            .await;

        res.assert_status_bad_request();
    });
}

#[test]
fn delete_trigger_removes_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([vec![polling_trigger_row()]])
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }]);

        let server = make_server(db.into_connection());
        let res = server
            .delete(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["deleted"], true);
    });
}

#[test]
fn delete_trigger_not_found_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = ProjectTriggerId::now_v7();

        let db = with_project_edit(with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id))
            .append_query_results([Vec::<project_trigger::Model>::new()]);

        let server = make_server(db.into_connection());
        let res = server
            .delete(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

// fire_now is not integration-tested here because it calls resolve_head which
// makes actual git network requests — it will be exercised by E2E smoke tests.

#[test]
fn create_project_seeds_default_polling_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let created_project = project::Model {
            id: project_id(),
            organization: org_id(),
            name: "new-project".into(),
            active: true,
            display_name: "New Project".into(),
            description: "".into(),
            repository: "https://github.com/test/repo".into(),
            evaluation_wildcard: "*".into(),
            last_evaluation: None,
            last_check_at: test_date(),
            force_evaluation: false,
            created_by: user_id(),
            created_at: test_date(),
            managed: false,
            keep_evaluations: 30,
        };

        let seeded_trigger = project_trigger::Model {
            id: trigger_id(),
            project: project_id(),
            trigger_type: 0,  // Polling
            concurrency: 3,   // Skip
            config: serde_json::json!({"interval_secs": 300}),
            active: true,
            last_fired_at: None,
            created_at: test_date(),
            updated_at: test_date(),
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_org: SELECT org
            .append_query_results([vec![org()]])
            // load_org: SELECT org_user (require CreateProject permission)
            .append_query_results([vec![admin_membership()]])
            // check existing project: returns empty
            .append_query_results([Vec::<project::Model>::new()])
            // INSERT project RETURNING
            .append_query_results([vec![created_project]])
            // INSERT trigger RETURNING
            .append_query_results([vec![seeded_trigger]]);

        let server = make_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-org")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "new-project",
                "display_name": "New Project",
                "description": "",
                "repository": "https://github.com/test/repo",
                "evaluation_wildcard": "*"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], project_id().to_string());
    });
}
