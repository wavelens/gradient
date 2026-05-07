/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /commits/{commit}` authorization.
//!
//! Regression coverage for issue #88 (IDOR): the handler must only return
//! commit metadata when the caller can reach the commit through an
//! evaluation in an organization they belong to (or the org is public).
//! Both authenticated non-members and unauthenticated callers must receive
//! `404` so existence isn't leaked.
//!
//! DB query sequence after the move to `authorize_optional`:
//!   Authenticated callers:
//!     1. SELECT session  (jwt decode)
//!     2. UPDATE session  (last_used_at, returning)
//!     3. SELECT user
//!   Then for the commit handler (both auth states):
//!     4. SELECT commit
//!     5. SELECT evaluations (filtered by commit)
//!     6. SELECT projects    (only if any eval has project_id)
//!     7. SELECT direct_builds (only if any eval has no project_id)
//!     8. SELECT organizations (filtered by collected ids)
//!   Then membership probe (only when no org is public AND caller is authenticated):
//!     9. SELECT organization_user

use axum_test::TestServer;
use chrono::{Duration, Utc};
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
use test_support::fixtures::{commit_id, eval_at, org, org_id, project_id, test_date, user, user_id};
use test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use web::create_router;

const JWT_SECRET: &str = "test-commits-jwt-secret";

fn commit_url() -> String {
    format!("/api/v1/commits/{}", commit_id())
}

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

fn other_org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a0").unwrap())
}

fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000050").unwrap())
}

fn direct_build_id() -> DirectBuildId {
    DirectBuildId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000060").unwrap())
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
        concurrency: 3,
        sign_cache: true,
    }
}

fn commit_row() -> entity::commit::Model {
    entity::commit::Model {
        id: commit_id(),
        message: "feat: something".into(),
        hash: vec![0xab; 20],
        author: None,
        author_name: "Tester".into(),
    }
}

fn public_org() -> entity::organization::Model {
    entity::organization::Model {
        public: true,
        ..org()
    }
}

fn membership_row() -> entity::organization_user::Model {
    entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_VIEW_ID,
    }
}

fn direct_build_row() -> entity::direct_build::Model {
    entity::direct_build::Model {
        id: direct_build_id(),
        organization: org_id(),
        evaluation: eval_id(),
        derivation: "/nix/store/aaaa-pkg.drv".into(),
        repository_path: "/nix/store/aaaa-pkg".into(),
        created_by: user_id(),
        created_at: test_date(),
    }
}

// ── Server factory ────────────────────────────────────────────────────────────

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
    });
    TestServer::new(create_router(state))
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Anonymous caller: commit reachable through a public-org project → 200.
#[test]
fn anon_can_read_commit_in_public_org() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![eval_at(eval_id(), 0)]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![public_org()]]);
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["id"], commit_id().to_string());
    });
}

/// Anonymous caller: commit reachable only through a private org → 404.
#[test]
fn anon_cannot_read_commit_in_private_org() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![eval_at(eval_id(), 0)]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org()]]); // private
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

/// Authenticated org member: commit reachable through a private org they
/// belong to → 200.
#[test]
fn member_can_read_commit_in_private_org() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![eval_at(eval_id(), 0)]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org()]])
            .append_query_results([vec![membership_row()]]);
        let server = make_server(db.into_connection());

        let res = server
            .get(&commit_url())
            .add_header("authorization", format!("Bearer {}", token))
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["id"], commit_id().to_string());
    });
}

/// Authenticated user with no membership in any org that owns the commit's
/// evaluation → 404 (must not leak existence).
#[test]
fn non_member_cannot_read_commit() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // Commit reachable through a project in `other_org_id` — caller has no
        // membership there.
        let foreign_project = entity::project::Model {
            organization: other_org_id(),
            ..project_row()
        };
        let foreign_org = entity::organization::Model {
            id: other_org_id(),
            ..org()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![eval_at(eval_id(), 0)]])
            .append_query_results([vec![foreign_project]])
            .append_query_results([vec![foreign_org]])
            .append_query_results([Vec::<entity::organization_user::Model>::new()]);
        let server = make_server(db.into_connection());

        let res = server
            .get(&commit_url())
            .add_header("authorization", format!("Bearer {}", token))
            .await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

/// Commit referenced only via a `direct_build` (evaluation has no project):
/// org member must still see it.
#[test]
fn member_can_read_commit_referenced_via_direct_build() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let direct_eval = entity::evaluation::Model {
            project: None,
            ..eval_at(eval_id(), 0)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![direct_eval]])
            // No project ids → projects query is skipped.
            .append_query_results([vec![direct_build_row()]])
            .append_query_results([vec![org()]])
            .append_query_results([vec![membership_row()]]);
        let server = make_server(db.into_connection());

        let res = server
            .get(&commit_url())
            .add_header("authorization", format!("Bearer {}", token))
            .await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["id"], commit_id().to_string());
    });
}

/// Commit row doesn't exist → 404 with no further DB lookups needed.
#[test]
fn nonexistent_commit_returns_404() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<entity::commit::Model>::new()]);
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

/// Commit row exists but no evaluation references it (orphan or
/// race-condition cleanup) → 404.
#[test]
fn commit_without_evaluation_returns_404() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![commit_row()]])
            .append_query_results([Vec::<entity::evaluation::Model>::new()]);
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}
