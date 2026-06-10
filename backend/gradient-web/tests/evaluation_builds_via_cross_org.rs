/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration test for `GET /evals/{evaluation}/builds` where the follower's
//! leader belongs to a different organisation.
//!
//! Mock query sequence (private org_b, authenticated user, member of org_b only):
//!   Auth prefix (authorize_optional):
//!     1. SELECT session           (decode_jwt)
//!     2. SELECT session           (update last_used_at, returning)
//!     3. SELECT user
//!   EvalAccessContext::load:
//!     4. SELECT evaluation        (org_b's eval)
//!     5. SELECT project           (org_b's project)
//!     6. SELECT organization      (org_b - private)
//!     7. SELECT organization_user (org_b × user → member)
//!   get_evaluation_builds:
//!     8. SELECT builds            (filter by evaluation = follower_eval_id)
//!     9. SELECT builds            (filter by id in [leader_build_id]) - leader-row dereference
//!    10. SELECT derivations       (filter by id in [leader_drv_id]) - no org filter
//!    11. SELECT derivation_outputs (filter by derivation in [leader_drv_id]) - empty

use axum_test::TestServer;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::*;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::{RuntimeConfig, SecretString};
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
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
    OrganizationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000001").unwrap())
}
fn follower_org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000002").unwrap())
}
#[allow(dead_code)]
fn leader_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000003").unwrap())
}
fn follower_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000004").unwrap())
}
fn leader_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000005").unwrap())
}
fn follower_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000006").unwrap())
}
fn leader_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000007").unwrap())
}
fn follower_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000008").unwrap())
}
fn leader_drv_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000009").unwrap())
}
fn follower_drv_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000010").unwrap())
}
fn follower_membership_id() -> OrganizationUserId {
    OrganizationUserId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000011").unwrap())
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

fn follower_build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: follower_build_id(),
        evaluation: follower_eval_id(),
        derivation: follower_drv_id(),
        status: BuildStatus::Queued,
        via: Some(leader_build_id()),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn leader_build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: leader_build_id(),
        evaluation: leader_eval_id(),
        derivation: leader_drv_id(),
        status: BuildStatus::Building,
        worker: Some("worker-1".into()),
        created_at: test_date(),
        updated_at: test_date() + chrono::Duration::seconds(30),
        ..Default::default()
    }
}

fn leader_derivation_row() -> gradient_entity::derivation::Model {
    gradient_entity::derivation::Model {
        id: leader_drv_id(),
        organization: leader_org_id(),
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        name: "hello-2.12.1".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn follower_org_membership() -> gradient_entity::organization_user::Model {
    gradient_entity::organization_user::Model {
        id: follower_membership_id(),
        organization: follower_org_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_VIEW_ID,
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
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
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

// ── Test ──────────────────────────────────────────────────────────────────────

/// A follower build whose leader belongs to a different organisation must still
/// surface the leader's `id` and `status` in `GET /evals/{eval}/builds`.
/// The leader-row dereference does not filter on org, so the cross-org fetch
/// succeeds and the response reflects the leader's live `Building` status.
#[test]
fn evaluation_builds_resolves_cross_org_leader_row() {
    let sid = SessionId::now_v7();
    let token = make_token(sid);
    let session = live_session(sid);

    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Auth prefix (authorize_optional)
            .append_query_results([vec![session.clone()]]) // 1. SELECT session
            .append_query_results([vec![session]]) // 2. UPDATE session (returning)
            .append_query_results([vec![user()]]) // 3. SELECT user
            // EvalAccessContext::load
            .append_query_results([vec![evaluation_row(
                follower_eval_id(),
                follower_project_id(),
            )]]) // 4. SELECT evaluation
            .append_query_results([vec![project_row(follower_project_id(), follower_org_id())]]) // 5. SELECT project
            .append_query_results([vec![private_org(follower_org_id(), "org-b")]]) // 6. SELECT organization
            .append_query_results([vec![follower_org_membership()]]) // 7. SELECT organization_user (member)
            // get_evaluation_builds
            .append_query_results([vec![follower_build_row()]]) // 8. SELECT builds (eval = follower_eval_id)
            .append_query_results([vec![leader_build_row()]]) // 9. SELECT builds (id in [leader_build_id])
            .append_query_results([vec![leader_derivation_row()]]) // 10. SELECT derivations (id in [leader_drv_id])
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()]) // 11. SELECT derivation_outputs (empty)
            .into_connection();

        let server = make_server(db);
        let res = server
            .get(&format!("/api/v1/evals/{}/builds", follower_eval_id()))
            .add_header(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
            )
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        let builds = body["message"]["builds"].as_array().expect("builds array");
        assert_eq!(builds.len(), 1);

        let item = &builds[0];
        assert_eq!(
            item["id"],
            leader_build_id().to_string(),
            "follower row must surface the cross-org leader's id"
        );
        assert_eq!(item["status"], "Building", "must surface leader's status");
        assert_eq!(body["message"]["total"], 1);
        assert_eq!(body["message"]["active_count"], 1);
    });
}
