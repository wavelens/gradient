/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for cross-cache follower-org access on `GET /builds/{id}/log`.
//!
//! A follower build points at a leader build via the `via` column. A user
//! who is a member of the follower's org (but NOT the leader's org) must be
//! able to read the leader build's log endpoint. A member of an entirely
//! unrelated org (no follower link) must receive 404.
//!
//! Mock query sequence - positive case (private leader-org, follower-org member):
//!   Auth prefix (authorize_optional):
//!     1. SELECT session           (decode_jwt)
//!     2. SELECT session           (update last_used_at, returning)
//!     3. SELECT user
//!   BuildAccessContext::load_unguarded:
//!     4. SELECT build             (leader build by id)
//!     5. SELECT evaluation        (leader build's evaluation)
//!     6. SELECT project           (leader evaluation's project)
//!     7. SELECT organization      (leader org - private)
//!   Direct membership check (private org → is_org_member → load_org_membership):
//!     8. SELECT organization_user (leader org × user → empty → not a direct member)
//!   follower_orgs_accessible:
//!     9. SELECT build             (WHERE via = leader_build_id)
//!    10. SELECT evaluation        (WHERE id IN [follower_eval_id])
//!    11. SELECT project           (follower evaluation's project)
//!    12. SELECT organization_user (follower org × user → Some → access granted)
//!
//! Negative case (unrelated org, no follower link):
//!   Auth prefix: queries 1–3 (same as above)
//!   BuildAccessContext::load_unguarded: queries 4–7 (same)
//!   Direct membership check: query 8 → empty
//!   follower_orgs_accessible: query 9 → empty vec → short-circuit → 404

use axum_test::TestServer;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::*;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{test_date, user, user_id};
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::web::{TEST_JWT_SECRET, live_session, make_token};
use uuid::Uuid;
use gradient_web::create_router;

// ── Stable UUIDs ──────────────────────────────────────────────────────────────

fn leader_org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000001").unwrap())
}
fn follower_org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000002").unwrap())
}
fn leader_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000003").unwrap())
}
fn follower_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000004").unwrap())
}
fn leader_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000005").unwrap())
}
fn follower_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000006").unwrap())
}
fn leader_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000007").unwrap())
}
fn follower_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000008").unwrap())
}
fn follower_membership_id() -> OrganizationUserId {
    OrganizationUserId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000009").unwrap())
}
fn session_id() -> SessionId {
    SessionId::now_v7()
}

// ── Fixture rows ──────────────────────────────────────────────────────────────

fn private_org(id: OrganizationId, slug: &str) -> gradient_entity::organization::Model {
    gradient_entity::organization::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn project_row(id: ProjectId, org: OrganizationId) -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id,
        organization: org,
        name: "proj".into(),
        display_name: "Proj".into(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        active: true,
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

fn evaluation_row(id: EvaluationId, project: ProjectId) -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id,
        project: Some(project),
        repository: "https://example.com/repo".into(),
        commit: CommitId::now_v7(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn leader_build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: leader_build_id(),
        evaluation: leader_eval_id(),
        derivation: DerivationId::now_v7(),
        status: BuildStatus::Completed,
        build_time_ms: Some(1000),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn follower_build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: follower_build_id(),
        evaluation: follower_eval_id(),
        derivation: DerivationId::now_v7(),
        status: BuildStatus::Completed,
        build_time_ms: Some(1000),
        via: Some(leader_build_id()),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn follower_org_membership() -> gradient_entity::organization_user::Model {
    gradient_entity::organization_user::Model {
        id: follower_membership_id(),
        organization: follower_org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_VIEW_ID,
    }
}

// ── Server factory ─────────────────────────────────────────────────────────────

fn make_server(db: sea_orm::DatabaseConnection) -> TestServer {
    let cli = gradient_test_support::cli::test_cli();
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
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_core::db::NoReactor),
    });
    TestServer::new(create_router(state))
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A member of the follower-org can read the leader build's log even though
/// they have no direct membership in the leader's (private) org.
#[test]
fn follower_org_member_gets_leader_log() {
    let sid = session_id();
    let token = make_token(sid);
    let session = live_session(sid);

    run(async {
        let follower_eval = evaluation_row(follower_eval_id(), follower_project_id());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Auth prefix (authorize_optional)
            .append_query_results([vec![session.clone()]]) // 1. SELECT session
            .append_query_results([vec![session]]) // 2. UPDATE session (returning)
            .append_query_results([vec![user()]]) // 3. SELECT user
            // BuildAccessContext::load_unguarded
            .append_query_results([vec![leader_build_row()]]) // 4. SELECT build
            .append_query_results([vec![evaluation_row(leader_eval_id(), leader_project_id())]]) // 5. SELECT evaluation
            .append_query_results([vec![project_row(leader_project_id(), leader_org_id())]]) // 6. SELECT project
            .append_query_results([vec![private_org(leader_org_id(), "leader-org")]]) // 7. SELECT organization
            // Direct membership check → not a member of leader-org
            .append_query_results([Vec::<gradient_entity::organization_user::Model>::new()]) // 8. SELECT organization_user (empty)
            // follower_orgs_accessible
            .append_query_results([vec![follower_build_row()]]) // 9. SELECT build WHERE via=leader
            .append_query_results([vec![follower_eval]]) // 10. SELECT evaluation WHERE id IN [...]
            .append_query_results([vec![project_row(follower_project_id(), follower_org_id())]]) // 11. SELECT project
            .append_query_results([vec![follower_org_membership()]]) // 12. SELECT organization_user (member of follower-org)
            .into_connection();

        let server = make_server(db);
        let res = server
            .get(&format!("/api/v1/builds/{}/log", leader_build_id()))
            .add_header(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
            )
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(
            body["error"], false,
            "follower-org member must get 200 with log"
        );
    });
}

/// A member of an unrelated org (no follower build pointing at this leader)
/// must receive 404 - the leader build is invisible to them.
#[test]
fn unrelated_org_member_cannot_read_leader_log() {
    let sid = session_id();
    let token = make_token(sid);
    let session = live_session(sid);

    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Auth prefix
            .append_query_results([vec![session.clone()]]) // 1. SELECT session
            .append_query_results([vec![session]]) // 2. UPDATE session
            .append_query_results([vec![user()]]) // 3. SELECT user
            // BuildAccessContext::load_unguarded
            .append_query_results([vec![leader_build_row()]]) // 4. SELECT build
            .append_query_results([vec![evaluation_row(leader_eval_id(), leader_project_id())]]) // 5. SELECT evaluation
            .append_query_results([vec![project_row(leader_project_id(), leader_org_id())]]) // 6. SELECT project
            .append_query_results([vec![private_org(leader_org_id(), "leader-org")]]) // 7. SELECT organization
            // Direct membership check → not a member of leader-org
            .append_query_results([Vec::<gradient_entity::organization_user::Model>::new()]) // 8. SELECT organization_user (empty)
            // follower_orgs_accessible → no followers → short-circuit
            .append_query_results([Vec::<gradient_entity::build::Model>::new()]) // 9. SELECT build WHERE via=leader (empty)
            .into_connection();

        let server = make_server(db);
        let res = server
            .get(&format!("/api/v1/builds/{}/log", leader_build_id()))
            .add_header(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
            )
            .await;

        res.assert_status_not_found();
        let body: serde_json::Value = res.json();
        assert_eq!(body["error"], true, "unrelated org member must get 404");
    });
}
