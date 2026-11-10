/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::*;
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, public_project};
use gradient_test_support::log_storage::{InMemoryLogStorage, ReadErrorLogStorage};
use gradient_test_support::state::TestState;
use sea_orm::DatabaseConnection;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

fn build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb1))
}
fn shared_build_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::from_u128(0xa1))
}
fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}
fn attempt_id() -> BuildAttemptId {
    BuildAttemptId::new(Uuid::from_u128(0xc1))
}

const FAILED_NIX_LOG: &str = "\x1b[31;1merror:\x1b[0m builder failed\nFor full logs, run:\n  nix log /nix/store/abc-hello.drv\n";

fn mock_database_connection_with_build_log_endpoint_rows(status: BuildStatus) -> DatabaseConnection {
    mock_db_with_build_access_rows(status)
        .append_query_results([vec![gradient_entity::build_attempt::Model {
            id: attempt_id(),
            derivation_build: shared_build_id(),
            ..Default::default()
        }]])
        .into_connection()
}

fn mock_db_with_build_access_rows(status: BuildStatus) -> sea_orm::MockDatabase {
    mock_db()
        .append_query_results([vec![gradient_entity::build_job::Model {
            id: build_job_id(),
            evaluation: evaluation_id(),
            derivation_build: shared_build_id(),
            ..Default::default()
        }]])
        .append_query_results([vec![gradient_entity::derivation_build::Model {
            id: shared_build_id(),
            status,
            ..Default::default()
        }]])
        .append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![gradient_entity::task::Model {
            id: task_id(),
            project: project_id(),
            ..Default::default()
        }]])
        .append_query_results([vec![public_project()]])
}

#[tokio::test]
async fn the_attempts_list_names_the_worker_of_each_attempt() {
    let retried = gradient_entity::build_attempt::Model {
        id: BuildAttemptId::new(Uuid::from_u128(0xc2)),
        derivation_build: shared_build_id(),
        dispatched_job: Some(DispatchedJobId::new(Uuid::from_u128(0xd2))),
        ..Default::default()
    };
    let first = gradient_entity::build_attempt::Model {
        id: BuildAttemptId::new(Uuid::from_u128(0xc1)),
        derivation_build: shared_build_id(),
        outcome: gradient_entity::build_attempt::AttemptOutcome::Failed,
        ..Default::default()
    };
    let job = gradient_entity::dispatched_job::Model {
        id: DispatchedJobId::new(Uuid::from_u128(0xd2)),
        worker_id: "worker-b".into(),
        ..Default::default()
    };
    let db = mock_db_with_build_access_rows(BuildStatus::Completed)
        .append_query_results([vec![(retried, Some(job)), (first, None)]])
        .into_connection();

    let response = TestState::new()
        .web_db(db)
        .server()
        .get(&format!("/api/v1/builds/{}/attempts", build_job_id()))
        .await;

    response.assert_status_ok();
    let attempts = &response.json::<serde_json::Value>()["message"];
    assert_eq!(attempts[0]["worker"], "worker-b");
    assert_eq!(attempts[1]["outcome"], "Failed");
    assert!(attempts[1]["worker"].is_null());
}

#[tokio::test]
async fn an_attempt_of_another_build_is_not_found() {
    let db = mock_db_with_build_access_rows(BuildStatus::Completed)
        .append_query_results([vec![gradient_entity::build_attempt::Model {
            id: BuildAttemptId::new(Uuid::from_u128(0xc9)),
            derivation_build: DerivationBuildId::new(Uuid::from_u128(0xa9)),
            ..Default::default()
        }]])
        .into_connection();

    let response = TestState::new()
        .web_db(db)
        .server()
        .get(&format!(
            "/api/v1/builds/{}/log?attempt={}",
            build_job_id(),
            Uuid::from_u128(0xc9)
        ))
        .await;

    assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
}

fn server_with_failed_build_log() -> axum_test::TestServer {
    let logs = InMemoryLogStorage::new();
    logs.seed(attempt_id(), FAILED_NIX_LOG);
    TestState::new()
        .web_db(mock_database_connection_with_build_log_endpoint_rows(
            BuildStatus::FailedPermanent,
        ))
        .log_storage(Arc::new(logs))
        .config(|c| c.server.frontend_url = "https://ci.example/".into())
        .server()
}

fn web_log_link() -> String {
    format!(
        "Full log: https://ci.example/project/test-project/log/{}?build={}\n",
        evaluation_id(),
        build_job_id()
    )
}

#[tokio::test]
async fn a_build_log_read_failure_is_a_server_error_not_an_empty_log() {
    let path = format!("/api/v1/builds/{}/log", build_job_id());
    let readable = TestState::new()
        .web_db(mock_database_connection_with_build_log_endpoint_rows(
            BuildStatus::Completed,
        ))
        .server()
        .get(&path)
        .await;
    let unreadable = TestState::new()
        .web_db(mock_database_connection_with_build_log_endpoint_rows(
            BuildStatus::Completed,
        ))
        .log_storage(Arc::new(ReadErrorLogStorage))
        .server()
        .get(&path)
        .await;

    readable.assert_status_ok();
    assert_eq!(unreadable.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn plain_text_log_of_a_failed_build_has_no_colour_codes_and_ends_with_the_web_link() {
    let res = server_with_failed_build_log()
        .get(&format!("/api/v1/builds/{}/log", build_job_id()))
        .add_header("accept", "text/plain")
        .await;

    res.assert_status_ok();
    assert!(
        res.header("content-type")
            .to_str()
            .unwrap_or_default()
            .starts_with("text/plain")
    );
    assert_eq!(
        res.text(),
        format!("error: builder failed\n{}", web_log_link())
    );
}

#[tokio::test]
async fn json_log_keeps_colour_codes_unless_plain_format_is_requested() {
    let path = format!("/api/v1/builds/{}/log", build_job_id());

    let ansi: Value = server_with_failed_build_log().get(&path).await.json();
    let plain: Value = server_with_failed_build_log()
        .get(&format!("{path}?format=plain"))
        .await
        .json();

    assert_eq!(
        ansi["message"],
        format!("\x1b[31;1merror:\x1b[0m builder failed\n{}", web_log_link())
    );
    assert_eq!(
        plain["message"],
        format!("error: builder failed\n{}", web_log_link())
    );
}

#[tokio::test]
async fn log_of_a_successful_build_has_no_web_link() {
    let logs = InMemoryLogStorage::new();
    logs.seed(attempt_id(), "built\n");
    let res = TestState::new()
        .web_db(mock_database_connection_with_build_log_endpoint_rows(
            BuildStatus::Completed,
        ))
        .log_storage(Arc::new(logs))
        .server()
        .get(&format!("/api/v1/builds/{}/log", build_job_id()))
        .add_header("accept", "text/plain")
        .await;

    res.assert_status_ok();
    assert_eq!(res.text(), "built\n");
}
