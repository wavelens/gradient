/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /evals/{evaluation}/artefacts` (issue #234, task 12).
//!
//! Mock query sequence (public org, anonymous): evaluation → project →
//! organization → entry_points → builds → derivations → derivation_outputs →
//! build_products. Empty short-circuits skip the trailing queries.

use axum::http::StatusCode;
use axum_test::TestServer;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::ids::*;
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
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
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000004").unwrap())
}
fn build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000005").unwrap())
}
fn entry_point_id() -> EntryPointId {
    EntryPointId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000006").unwrap())
}
fn out_id() -> DerivationOutputId {
    DerivationOutputId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000007").unwrap())
}
fn lib_id() -> DerivationOutputId {
    DerivationOutputId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000008").unwrap())
}
fn product_a_id() -> BuildProductId {
    BuildProductId::new(Uuid::parse_str("40000000-0000-0000-0000-000000000009").unwrap())
}
fn product_b_id() -> BuildProductId {
    BuildProductId::new(Uuid::parse_str("40000000-0000-0000-0000-00000000000a").unwrap())
}
fn product_c_id() -> BuildProductId {
    BuildProductId::new(Uuid::parse_str("40000000-0000-0000-0000-00000000000b").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("40000000-0000-0000-0000-00000000000c").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

const HASH: &str = "cccccccccccccccccccccccccccccccc";

fn org_row(public: bool) -> entity::organization::Model {
    entity::organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Org".into(),
        description: String::new(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        public,
        hide_build_requests: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
    }
}

fn project_row() -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://example.com/repo".into(),
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

fn evaluation_row() -> entity::evaluation::Model {
    entity::evaluation::Model {
        id: eval_id(),
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: CommitId::now_v7(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
        repo_check_id: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
    }
}

fn derivation_row() -> entity::derivation::Model {
    entity::derivation::Model {
        id: derivation_id(),
        organization: org_id(),
        hash: HASH.into(),
        name: "hello-2.12.1".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
    }
}

fn build_row() -> entity::build::Model {
    entity::build::Model {
        id: build_id(),
        evaluation: eval_id(),
        derivation: derivation_id(),
        status: BuildStatus::Completed,
        log_id: None,
        build_time_ms: None,
        worker: None,
        via: None,
        external_cached: false,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn entry_point_row() -> entity::entry_point::Model {
    entity::entry_point::Model {
        id: entry_point_id(),
        project: project_id(),
        evaluation: eval_id(),
        build: build_id(),
        eval: "checks.x86_64-linux.foo".into(),
        created_at: test_date(),
        repo_check_id: None,
    }
}

fn output_out() -> entity::derivation_output::Model {
    entity::derivation_output::Model {
        id: out_id(),
        derivation: derivation_id(),
        name: "out".into(),
        hash: HASH.into(),
        package: "hello-2.12.1".into(),
        ca: None,
        nar_size: None,
        is_cached: true,
        cached_path: None,
        created_at: test_date(),
    }
}

fn output_lib() -> entity::derivation_output::Model {
    entity::derivation_output::Model {
        id: lib_id(),
        derivation: derivation_id(),
        name: "lib".into(),
        hash: HASH.into(),
        package: "hello-2.12.1-lib".into(),
        ca: None,
        nar_size: None,
        is_cached: true,
        cached_path: None,
        created_at: test_date(),
    }
}

fn product_for(id: BuildProductId, output: DerivationOutputId, path: &str) -> entity::build_product::Model {
    entity::build_product::Model {
        id,
        derivation_output: output,
        file_type: "doc".into(),
        subtype: "html".into(),
        name: "manual".into(),
        path: path.into(),
        size: Some(12345),
        created_at: test_date(),
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

/// Evaluation with no entry points returns an empty tree (early-return path).
#[test]
fn empty_eval_returns_empty_tree() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row()]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row(true)]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/artefacts", eval_id()))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["evaluation"], eval_id().to_string());
        assert!(body["message"]["entry_points"].as_array().unwrap().is_empty());
    });
}

/// Tree shape: one entry point with two outputs, three products spread across them.
#[test]
fn returns_full_tree_grouped_by_entry_point_and_output() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row()]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row(true)]])
            .append_query_results([vec![entry_point_row()]])
            .append_query_results([vec![build_row()]])
            .append_query_results([vec![derivation_row()]])
            .append_query_results([vec![output_out(), output_lib()]])
            .append_query_results([vec![
                product_for(product_a_id(), out_id(), "share/doc/a.html"),
                product_for(product_b_id(), out_id(), "share/doc/b.html"),
                product_for(product_c_id(), lib_id(), "share/lib/c.html"),
            ]])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/artefacts", eval_id()))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();

        let entry_points = body["message"]["entry_points"].as_array().unwrap();
        assert_eq!(entry_points.len(), 1);

        let ep = &entry_points[0];
        assert_eq!(ep["attr"], "checks.x86_64-linux.foo");
        assert_eq!(
            ep["derivation"],
            format!("/nix/store/{}-hello-2.12.1.drv", HASH)
        );

        let outputs = ep["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0]["name"], "lib");
        assert_eq!(
            outputs[0]["store_path"],
            format!("/nix/store/{}-hello-2.12.1-lib", HASH)
        );
        assert_eq!(outputs[0]["products"].as_array().unwrap().len(), 1);
        assert_eq!(outputs[0]["products"][0]["id"], product_c_id().to_string());
        assert_eq!(outputs[0]["products"][0]["type"], "doc");
        assert_eq!(outputs[0]["products"][0]["subtype"], "html");
        assert_eq!(outputs[0]["products"][0]["size"], 12345);

        assert_eq!(outputs[1]["name"], "out");
        let out_products = outputs[1]["products"].as_array().unwrap();
        assert_eq!(out_products.len(), 2);
        assert_eq!(out_products[0]["path"], "share/doc/a.html");
        assert_eq!(out_products[1]["path"], "share/doc/b.html");
    });
}

/// Missing evaluation returns 404 (mapped via EvalAccessContext::load -> not_found).
#[test]
fn missing_eval_returns_404() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/artefacts", eval_id()))
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}

/// Public org allows anonymous read (no Bearer token).
#[test]
fn public_org_allows_anonymous() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row()]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row(true)]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/artefacts", eval_id()))
            .await;

        res.assert_status_ok();
    });
}

/// Private org rejects anonymous with 404 (not 403 — same as other eval endpoints).
#[test]
fn private_org_rejects_anonymous() {
    rt().block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row()]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row(false)]])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/artefacts", eval_id()))
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}
