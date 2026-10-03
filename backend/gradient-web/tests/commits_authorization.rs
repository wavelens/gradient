/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::ids::*;
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{
    commit_id, eval_at, project, project_id, task_id, test_date, user, user_id,
};
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::{ConcurrencyPolicy, RuntimeConfig, SecretString, SessionId};
use gradient_web::create_router;
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const JWT_SECRET: &str = "test-commits-jwt-secret";

fn commit_url() -> String {
    format!("/api/v1/commits/{}", commit_id())
}

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

fn other_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a0").unwrap())
}

fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000050").unwrap())
}

fn task_row() -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::Skip,
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

fn public_project() -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        public: true,
        ..project()
    }
}

fn membership_row() -> gradient_entity::project_user::Model {
    gradient_entity::project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap()),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_VIEW_ID,
    }
}

fn make_server(db: sea_orm::DatabaseConnection) -> TestServer {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.server.base_dir).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    });
    TestServer::new(create_router(state).expect("router"))
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

#[tokio::test]
async fn anon_can_read_commit_in_public_project() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![public_project()]]);
    let server = make_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["id"], commit_id().to_string());
}

#[tokio::test]
async fn anon_cannot_read_commit_in_private_project() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project()]]);
    let server = make_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn member_can_read_commit_in_private_project() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project()]])
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
}

#[tokio::test]
async fn non_member_cannot_read_commit() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let foreign_task = gradient_entity::task::Model {
        project: other_project_id(),
        ..task_row()
    };
    let foreign_project = gradient_entity::project::Model {
        id: other_project_id(),
        ..project()
    };

    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![foreign_task]])
        .append_query_results([vec![foreign_project]])
        .append_query_results([Vec::<gradient_entity::project_user::Model>::new()]);
    let server = make_server(db.into_connection());

    let res = server
        .get(&commit_url())
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn commit_referenced_only_via_orphan_eval_returns_404() {
    let direct_eval = gradient_entity::evaluation::Model {
        task: None,
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
}

#[tokio::test]
async fn nonexistent_commit_returns_404() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()]);
    let server = make_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn commit_without_evaluation_returns_404() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![commit_row()]])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()]);
    let server = make_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}
