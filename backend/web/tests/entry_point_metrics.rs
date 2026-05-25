/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /projects/{org}/{project}/entry-point-metrics`.
//!
//! The metrics endpoint must surface a point whenever an `entry_point` row
//! exists for the requested `eval` attribute path - including the common case
//! where the owning evaluation is still in progress but the entry-point's
//! build has already finished (Completed or Substituted). The empty state on
//! the frontend should appear only when no `entry_point` row exists at all.

use axum_test::TestServer;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::ids::*;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000001").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000002").unwrap())
}
fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000003").unwrap())
}
fn build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000004").unwrap())
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000005").unwrap())
}
fn entry_point_id() -> EntryPointId {
    EntryPointId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000006").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000007").unwrap())
}
fn commit_id() -> CommitId {
    CommitId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000008").unwrap())
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

fn project_row(keep: i32) -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_evaluation: Some(eval_id()),
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: keep,
        concurrency: 3,
        sign_cache: true,
    }
}

fn entry_point_row(eval: &str) -> entity::entry_point::Model {
    entity::entry_point::Model {
        id: entry_point_id(),
        project: project_id(),
        evaluation: eval_id(),
        build: build_id(),
        eval: eval.into(),
        created_at: test_date(),
        repo_check_id: None,
    }
}

fn building_eval_row() -> entity::evaluation::Model {
    entity::evaluation::Model {
        id: eval_id(),
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        // The user's scenario: evaluation has NOT reached the terminal
        // Completed state yet - other builds are still running.
        status: EvaluationStatus::Building,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
    }
}

fn completed_build_row() -> entity::build::Model {
    entity::build::Model {
        id: build_id(),
        evaluation: eval_id(),
        derivation: derivation_id(),
        status: BuildStatus::Completed,
        log_id: None,
        build_time_ms: Some(12_345),
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

fn substituted_build_row() -> entity::build::Model {
    entity::build::Model {
        status: BuildStatus::Substituted,
        build_time_ms: None,
        worker: None,
        ..completed_build_row()
    }
}

/// Reproduces the user-reported bug: the evaluation is still Building (not yet
/// Completed), but the entry-point's build has already finished. The endpoint
/// must return one point so the frontend can render the chart instead of the
/// empty state.
#[test]
fn returns_point_when_eval_is_in_progress_but_build_is_completed() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(10)]])
            .append_query_results([vec![entry_point_row("packages.x86_64-linux.hello")]])
            .append_query_results([vec![building_eval_row()]])
            .append_query_results([vec![completed_build_row()]])
            .append_query_results([Vec::<entity::derivation_dependency::Model>::new()])
            .append_query_results([Vec::<entity::derivation_output::Model>::new()])
            .append_query_results([Vec::<entity::derivation_output::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-metrics")
            .add_query_param("eval", "packages.x86_64-linux.hello")
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let points = body["message"]["points"].as_array().expect("points array");
        assert_eq!(
            points.len(),
            1,
            "expected one metric point for the completed build, got: {body:#?}"
        );
        assert_eq!(points[0]["build_status"], "Completed");
        assert_eq!(points[0]["build_time_ms"], 12_345);
    });
}

/// Mirror of the Completed test but for Substituted entries - these have
/// `build_time_ms = None` and need to surface in the response so the recent
/// fix (#119) keeps working end-to-end.
#[test]
fn returns_point_when_eval_is_in_progress_but_build_is_substituted() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(10)]])
            .append_query_results([vec![entry_point_row("packages.x86_64-linux.hello")]])
            .append_query_results([vec![building_eval_row()]])
            .append_query_results([vec![substituted_build_row()]])
            .append_query_results([Vec::<entity::derivation_dependency::Model>::new()])
            .append_query_results([Vec::<entity::derivation_output::Model>::new()])
            .append_query_results([Vec::<entity::derivation_output::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-metrics")
            .add_query_param("eval", "packages.x86_64-linux.hello")
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let points = body["message"]["points"].as_array().expect("points array");
        assert_eq!(points.len(), 1, "expected one point, got: {body:#?}");
        assert_eq!(points[0]["build_status"], "Substituted");
        assert_eq!(points[0]["build_time_ms"], serde_json::Value::Null);
    });
}

/// Sanity check that the empty-state branch only fires when no matching
/// `entry_point` row exists at all - i.e. there is nothing to plot.
#[test]
fn returns_empty_points_when_no_entry_point_matches() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![public_org_row()]])
            .append_query_results([vec![project_row(10)]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get("/api/v1/projects/test-org/test-project/entry-point-metrics")
            .add_query_param("eval", "packages.x86_64-linux.unknown")
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let points = body["message"]["points"].as_array().expect("points array");
        assert!(points.is_empty());
    });
}
