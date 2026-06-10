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
//!     7. SELECT organizations (filtered by collected ids)
//!   Then membership probe (only when no org is public AND caller is authenticated):
//!     8. SELECT organization_user

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::ids::*;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::{RuntimeConfig, SecretString, SessionId};
use gradient_core::ServerState;
use gradient_core::db::{WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{
    commit_id, eval_at, org, org_id, project_id, test_date, user, user_id,
};
use gradient_test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use gradient_web::create_router;

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

fn live_session(id: SessionId) -> gradient_entity::session::Model {
    let now = Utc::now().naive_utc();
    gradient_entity::session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn other_org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a0").unwrap())
}

fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000050").unwrap())
}

fn project_row() -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

fn commit_row() -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id: commit_id(),
        message: "feat: something".into(),
        hash: vec![0xab; 20],
        author_name: "Tester".into(),
        ..Default::default()
    }
}

fn public_org() -> gradient_entity::organization::Model {
    gradient_entity::organization::Model {
        public: true,
        ..org()
    }
}

fn membership_row() -> gradient_entity::organization_user::Model {
    gradient_entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_VIEW_ID,
    }
}

// ── Server factory ────────────────────────────────────────────────────────────

fn make_server(db: sea_orm::DatabaseConnection) -> TestServer {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_core::db::NoReactor),
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

        // Commit reachable through a project in `other_org_id` - caller has no
        // membership there.
        let foreign_project = gradient_entity::project::Model {
            organization: other_org_id(),
            ..project_row()
        };
        let foreign_org = gradient_entity::organization::Model {
            id: other_org_id(),
            ..org()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![eval_at(eval_id(), 0)]])
            .append_query_results([vec![foreign_project]])
            .append_query_results([vec![foreign_org]])
            .append_query_results([Vec::<gradient_entity::organization_user::Model>::new()]);
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

/// Evaluation referencing the commit has no project (legacy direct-build
/// row before issue #234) → 404, since project is required to resolve org.
#[test]
fn commit_referenced_only_via_orphan_eval_returns_404() {
    run(async {
        let direct_eval = gradient_entity::evaluation::Model {
            project: None,
            ..eval_at(eval_id(), 0)
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![direct_eval]]);
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

/// Commit row doesn't exist → 404 with no further DB lookups needed.
#[test]
fn nonexistent_commit_returns_404() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::commit::Model>::new()]);
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
            .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()]);
        let server = make_server(db.into_connection());

        let res = server.get(&commit_url()).await;
        res.assert_status_not_found();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}
