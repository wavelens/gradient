/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /projects/{org}/{project}/entry-point-downloads`
//! (issue #185).
//!
//! The endpoint resolves the entry point against the evaluation pinned in
//! `project.last_evaluation` — i.e. the evaluation tied to the project's
//! newest commit — rather than the most recently *completed* evaluation. A
//! retriggered run for an older commit must not shadow the latest one.

use axum::http::StatusCode;
use axum_test::TestServer;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::ids::*;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000001").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000002").unwrap())
}
fn newest_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000003").unwrap())
}
fn build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000004").unwrap())
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000005").unwrap())
}
fn entry_point_id() -> EntryPointId {
    EntryPointId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000006").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000007").unwrap())
}
fn newest_commit_id() -> CommitId {
    CommitId::new(Uuid::parse_str("50000000-0000-0000-0000-000000000008").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn public_org_row() -> entity::organization::Model {
    entity::organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Org".into(),
        description: String::new(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        public: true,
        hide_build_requests: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
    }
}

fn project_row(last_evaluation: Option<EvaluationId>) -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_evaluation,
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

fn newest_eval_row() -> entity::evaluation::Model {
    entity::evaluation::Model {
        id: newest_eval_id(),
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: newest_commit_id(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        previous: None,
        next: None,
        // Older `created_at` than any retriggered run would have — the bug
        // scenario from #185: ordering by `created_at` would pick a different
        // (older-commit) evaluation, but `last_evaluation` must win.
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
        repo_check_id: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
    }
}

fn entry_point_row() -> entity::entry_point::Model {
    entity::entry_point::Model {
        id: entry_point_id(),
        project: project_id(),
        evaluation: newest_eval_id(),
        build: build_id(),
        eval: "packages.x86_64-linux.hello".into(),
        created_at: test_date(),
        repo_check_id: None,
    }
}

fn completed_build_row() -> entity::build::Model {
    entity::build::Model {
        id: build_id(),
        evaluation: newest_eval_id(),
        derivation: derivation_id(),
        status: BuildStatus::Completed,
        log_id: None,
        build_time_ms: Some(1_000),
        worker: Some("worker-1".into()),
        via: None,
        external_cached: false,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("nar store");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(
            gradient_core::types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
        ),
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Regression for #185: when a project has never had a successful evaluation
/// recorded, `last_evaluation` is `None` and the endpoint must 404 instead of
/// falling back to a search across all evaluations.
#[test]
fn returns_404_when_project_has_no_last_evaluation() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(None)]])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-downloads")
            .add_query_param("eval", "packages.x86_64-linux.hello")
            .add_query_param("filename", "manual")
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}

/// Happy path: with `project.last_evaluation` set, the handler resolves the
/// entry point against that evaluation and reaches the artefact-serving stage.
/// Empty `derivation_output` rows cause `serve_hydra_artifact` to return
/// `None`, surfacing as `404 File` — sufficient to prove the eval pinned in
/// `last_evaluation` was used.
#[test]
fn resolves_entry_point_against_project_last_evaluation() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(Some(newest_eval_id()))]])
            .append_query_results([vec![newest_eval_row()]])
            .append_query_results([vec![entry_point_row()]])
            .append_query_results([vec![completed_build_row()]])
            .append_query_results([Vec::<entity::derivation_output::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-downloads")
            .add_query_param("eval", "packages.x86_64-linux.hello")
            .add_query_param("filename", "manual")
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}

/// When the entry point for `eval` doesn't exist in the newest-commit
/// evaluation, the response is 404 — there is no fallback to older
/// evaluations that may still contain a matching entry point.
#[test]
fn returns_404_when_entry_point_missing_from_last_evaluation() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(Some(newest_eval_id()))]])
            .append_query_results([vec![newest_eval_row()]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-downloads")
            .add_query_param("eval", "packages.x86_64-linux.unknown")
            .add_query_param("filename", "manual")
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}
