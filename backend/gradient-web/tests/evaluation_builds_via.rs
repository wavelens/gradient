/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /evals/{evaluation}/builds`.
//!
//! Verifies that follower builds (`via IS NOT NULL`) surface the leader build's
//! row in the response - the follower's own `id`/`status`/`updated_at` are
//! stand-in placeholders and the frontend needs the leader's data to render the
//! live build and resolve the log endpoint to the right build id.
//!
//! Mock query sequence (public org → no auth/membership round-trip):
//!   1. SELECT evaluation             (EvalAccessContext::load)
//!   2. SELECT project                (EvalAccessContext::load)
//!   3. SELECT organization           (EvalAccessContext::load, public=true)
//!   4. SELECT builds                 (filter by evaluation)
//!   5. SELECT builds                 (filter by id in [via_ids]) - only when at least one follower exists
//!   6. SELECT derivations            (filter by id in [drv_ids])
//!   7. SELECT derivation_outputs     (filter by derivation in [drv_ids])
//!   8. SELECT build_products         (filter by derivation_output in [output_ids]) - skipped when no outputs

use axum_test::TestServer;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::ids::*;
use gradient_core::ServerState;
use gradient_core::db::{WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use uuid::Uuid;
use gradient_web::create_router;

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000001").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000002").unwrap())
}
fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000003").unwrap())
}
fn other_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000004").unwrap())
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000005").unwrap())
}
fn follower_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000006").unwrap())
}
fn leader_build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000007").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000008").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

const DRV_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12.1.drv";

fn org_row() -> gradient_entity::organization::Model {
    gradient_entity::organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Org".into(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn project_row() -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://example.com/repo".into(),
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

fn evaluation_row(id: EvaluationId) -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id,
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: CommitId::now_v7(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn derivation_row() -> gradient_entity::derivation::Model {
    gradient_entity::derivation::Model {
        id: derivation_id(),
        organization: org_id(),
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        name: "hello-2.12.1".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn follower_build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: follower_build_id(),
        evaluation: eval_id(),
        derivation: derivation_id(),
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
        evaluation: other_eval_id(),
        derivation: derivation_id(),
        status: BuildStatus::Building,
        worker: Some("worker-7".into()),
        created_at: test_date(),
        updated_at: test_date() + chrono::Duration::seconds(42),
        ..Default::default()
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
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_core::forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_core::db::NoReactor),
    })
}

/// A follower build in the evaluation must be returned with the leader's `id`,
/// `status`, `updated_at` and `build_time_ms` - not the follower's stand-in row.
/// The derivation path is shared, so `name` stays the same.
#[test]
fn follower_build_is_replaced_with_leader_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row(eval_id())]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row()]])
            .append_query_results([vec![follower_build_row()]])
            .append_query_results([vec![leader_build_row()]])
            .append_query_results([vec![derivation_row()]])
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/builds", eval_id()))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let builds = body["message"]["builds"].as_array().expect("builds array");
        assert_eq!(builds.len(), 1);

        let item = &builds[0];
        assert_eq!(
            item["id"],
            leader_build_id().to_string(),
            "follower row must surface the leader's id"
        );
        assert_eq!(item["status"], "Building");
        assert!(
            item["updated_at"]
                .as_str()
                .unwrap()
                .starts_with("2026-01-01T00:00:42"),
            "expected leader's updated_at, got {}",
            item["updated_at"]
        );
        assert_eq!(item["name"], DRV_PATH);
        assert_eq!(body["message"]["total"], 1);
        assert_eq!(body["message"]["active_count"], 1);
    });
}

/// A plain build (`via IS NULL`) skips the leader resolution entirely - the
/// extra SELECT must not be issued (MockDatabase would otherwise return junk
/// from the next-appended row and corrupt the response). Validates the
/// `leader_ids.is_empty()` short-circuit.
#[test]
fn plain_build_returns_own_row_without_extra_query() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let plain = gradient_entity::build::Model {
            via: None,
            status: BuildStatus::Completed,
            build_time_ms: Some(1234),
            updated_at: test_date() + chrono::Duration::seconds(7),
            ..follower_build_row()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluation_row(eval_id())]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row()]])
            .append_query_results([vec![plain]])
            .append_query_results([vec![derivation_row()]])
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .into_connection();

        let server = TestServer::new(create_router(make_state(db)));
        let res = server
            .get(&format!("/api/v1/evals/{}/builds", eval_id()))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let builds = body["message"]["builds"].as_array().expect("builds array");
        assert_eq!(builds.len(), 1);

        let item = &builds[0];
        assert_eq!(item["id"], follower_build_id().to_string());
        assert_eq!(item["status"], "Completed");
        assert_eq!(item["build_time_ms"], 1234);
    });
}
