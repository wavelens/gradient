/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the GET project evaluations endpoint.
//!
//! Verifies that the `EvaluationSummary` response shape includes the
//! `trigger` field and that it is correctly populated from the
//! `project_trigger` table.
//!
//! Auth + DB mock query sequence per request:
//!   1. SELECT session (jti lookup)
//!   2. SELECT session (UPDATE last_used_at — MockDatabase uses RETURNING path)
//!   3. SELECT user
//!   4. SELECT org (load_project)
//!   5. SELECT project (load_project)
//!   6. SELECT org_user (membership check)
//!
//! Then for the evaluations handler:
//!   7. SELECT evaluations (filtered by project, ordered, limited)
//!   8. SELECT project_triggers (batch-load trigger types) — skipped when no triggers
//!   9. SELECT commits (batch-load commit hashes)
//!  10. SELECT builds (batch-load build counts)
//!  11. SELECT entry_points (batch-load EP counts, including previous evals)
//!  12. SELECT builds for entry-point statuses (skipped when no EPs)

use axum_test::TestServer;
use chrono::Duration;
use chrono::Utc;
use entity::ids::*;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, SessionId, WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use test_support::cli::test_cli;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::fixtures::{
    commit_id, eval_at, org, org_id, project_id, test_date, user, user_id,
};
use test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use web::create_router;

const JWT_SECRET: &str = "test-eval-jwt-secret";
const BASE_URL: &str = "/api/v1/projects/test-org/test-project/evaluations";

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

fn live_session(id: SessionId) -> entity::session::Model {
    let now = Utc::now().naive_utc();
    entity::session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        revoked_at: None,
        user_agent: None,
        ip: None,
        remember_me: false,
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn trigger_id() -> ProjectTriggerId {
    ProjectTriggerId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap())
}

fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000020").unwrap())
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

fn admin_membership() -> entity::organization_user::Model {
    entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn polling_trigger_row() -> entity::project_trigger::Model {
    entity::project_trigger::Model {
        id: trigger_id(),
        project: project_id(),
        trigger_type: 0, // Polling
        config: serde_json::json!({"interval_secs": 60}),
        active: true,
        last_fired_at: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn commit_row() -> entity::commit::Model {
    entity::commit::Model {
        id: commit_id(),
        message: String::new(),
        hash: vec![0xab; 20],
        author: None,
        author_name: String::new(),
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
        webhooks: Arc::new(RecordingWebhookClient::new())
            as Arc<dyn gradient_core::ci::WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
    });
    TestServer::new(create_router(state))
}

// ── DB mock builders ───────────────────────────────────────────────────────────

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn with_project(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// When an evaluation has no trigger FK, `trigger` must be `null` in the response.
#[test]
fn evaluation_without_trigger_returns_null_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let eval = eval_at(eval_id(), 0); // trigger: None

        let db = with_project(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // 7. SELECT evaluations
        .append_query_results([vec![eval]])
        // 8. No trigger IDs — trigger query skipped; batch loads follow:
        // 9. SELECT commits
        .append_query_results([vec![commit_row()]])
        // 10. SELECT builds
        .append_query_results([Vec::<entity::build::Model>::new()])
        // 11. SELECT entry_points
        .append_query_results([Vec::<entity::entry_point::Model>::new()]);

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
        assert!(items[0]["trigger"].is_null(), "expected trigger to be null");
    });
}

/// When an evaluation has a trigger FK pointing to an existing `project_trigger`
/// row, the response must include the populated `EvaluationTriggerSummary`.
#[test]
fn evaluation_with_trigger_returns_trigger_summary() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let eval = entity::evaluation::Model {
            trigger: Some(trigger_id()),
            ..eval_at(eval_id(), 0)
        };

        let db = with_project(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // 7. SELECT evaluations
        .append_query_results([vec![eval]])
        // 8. SELECT project_triggers (batch for trigger_ids)
        .append_query_results([vec![polling_trigger_row()]])
        // 9. SELECT commits
        .append_query_results([vec![commit_row()]])
        // 10. SELECT builds
        .append_query_results([Vec::<entity::build::Model>::new()])
        // 11. SELECT entry_points
        .append_query_results([Vec::<entity::entry_point::Model>::new()]);

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

        let trigger = &items[0]["trigger"];
        assert!(!trigger.is_null(), "expected trigger to be present");
        assert_eq!(trigger["id"], trigger_id().to_string());
        assert_eq!(trigger["type"], "polling");
    });
}

/// When a trigger FK exists but the corresponding `project_trigger` row has been
/// deleted (ON DELETE SET NULL means this shouldn't happen for the FK itself, but
/// if it were somehow orphaned), or when the trigger row is simply not found in
/// the batch query result, the response must return `null` for `trigger`.
#[test]
fn evaluation_with_deleted_trigger_returns_null() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let eval = entity::evaluation::Model {
            trigger: Some(trigger_id()),
            ..eval_at(eval_id(), 0)
        };

        let db = with_project(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // 7. SELECT evaluations
        .append_query_results([vec![eval]])
        // 8. SELECT project_triggers — row not found (deleted trigger)
        .append_query_results([Vec::<entity::project_trigger::Model>::new()])
        // 9. SELECT commits
        .append_query_results([vec![commit_row()]])
        // 10. SELECT builds
        .append_query_results([Vec::<entity::build::Model>::new()])
        // 11. SELECT entry_points
        .append_query_results([Vec::<entity::entry_point::Model>::new()]);

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
        assert!(
            items[0]["trigger"].is_null(),
            "expected trigger to be null when row missing"
        );
    });
}
