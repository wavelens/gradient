/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the per-project `sign_cache` option.
//!
//! Mock-DB pattern shared with `triggers.rs`: manual Tokio runtime,
//! `axum_test::TestServer`, and `MockDatabase` because `#[tokio::test]`
//! macro expansion clashes with the local `core` crate name.

use axum_test::TestServer;
use chrono::{Duration, Utc};
use entity::{ids::*, organization_user, project, project_trigger, role, session};
use gradient_core::permissions::admin_mask;
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
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use web::create_router;

const JWT_SECRET: &str = "test-jwt-secret";

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

fn admin_role_row() -> role::Model {
    role::Model {
        id: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        organization: None,
        permission: admin_mask(),
        managed: false,
    }
}

fn project_with(sign_cache: bool) -> project::Model {
    project::Model {
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
        keep_evaluations: 30,
        concurrency: 1,
        sign_cache,
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

#[test]
fn get_project_includes_sign_cache() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![project_with(false)]])
            // is_org_member (Readable access)
            .append_query_results([vec![admin_membership()]])
            // has_permission(EditProject): membership + role
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            // has_permission(TriggerEvaluation): membership + role
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]]);

        let server = make_server(db.into_connection());
        let res = server
            .get("/api/v1/projects/test-org/test-project")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(
            body["message"]["sign_cache"], false,
            "GET response must echo project.sign_cache verbatim, got: {body}"
        );
    });
}

#[test]
fn patch_project_writes_sign_cache_false() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![project_with(true)]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![project_with(false)]]);

        let server = make_server(db.into_connection());
        let res = server
            .patch("/api/v1/projects/test-org/test-project")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "sign_cache": false }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}

#[test]
fn create_project_accepts_sign_cache_false() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let seeded_trigger = project_trigger::Model {
            id: ProjectTriggerId::now_v7(),
            project: project_id(),
            trigger_type: 0,
            config: serde_json::json!({"interval_secs": 300}),
            active: true,
            last_fired_at: None,
            created_at: test_date(),
            updated_at: test_date(),
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([Vec::<project::Model>::new()])
            .append_query_results([vec![project_with(false)]])
            .append_query_results([vec![seeded_trigger]]);

        let server = make_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-org")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "test-project",
                "display_name": "Test Project",
                "description": "",
                "repository": "https://github.com/test/repo",
                "wildcard": "*",
                "sign_cache": false
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], project_id().to_string());
    });
}
