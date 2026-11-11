/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::ids::*;
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, public_project};
use gradient_test_support::log_storage::{InMemoryLogStorage, ReadErrorLogStorage};
use gradient_test_support::state::TestState;
use sea_orm::{DatabaseConnection, MockDatabase};
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
fn retry_attempt_id() -> BuildAttemptId {
    BuildAttemptId::new(Uuid::from_u128(0xc2))
}

const FAILED_NIX_LOG: &str = "\x1b[31;1merror:\x1b[0m builder failed\nFor full logs, run:\n  nix log /nix/store/abc-hello.drv\n";

fn mock_database_connection_with_build_access_check_rows(status: BuildStatus) -> MockDatabase {
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

fn mock_database_connection_with_build_log_endpoint_rows(
    status: BuildStatus,
) -> DatabaseConnection {
    mock_database_connection_with_build_access_check_rows(status)
        .append_query_results([vec![gradient_entity::build_attempt::Model {
            id: attempt_id(),
            derivation_build: shared_build_id(),
            ..Default::default()
        }]])
        .into_connection()
}

fn failed_and_retried_attempts() -> Vec<gradient_entity::build_attempt::Model> {
    let at = |seconds: i64| {
        chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0)
            .expect("a valid timestamp")
            .naive_utc()
    };
    vec![
        gradient_entity::build_attempt::Model {
            id: attempt_id(),
            derivation_build: shared_build_id(),
            outcome: AttemptOutcome::Failed,
            reason: Some(AttemptFailureReason::BuilderNonzero),
            build_started_at: Some(at(0)),
            build_finished_at: Some(at(60)),
            created_at: at(0),
            ..Default::default()
        },
        gradient_entity::build_attempt::Model {
            id: retry_attempt_id(),
            derivation_build: shared_build_id(),
            dispatched_job: Some(DispatchedJobId::new(Uuid::from_u128(0xd2))),
            outcome: AttemptOutcome::Built,
            build_started_at: Some(at(120)),
            build_finished_at: Some(at(180)),
            created_at: at(120),
            ..Default::default()
        },
    ]
}

fn server_with_failed_and_retried_attempts(db: MockDatabase) -> axum_test::TestServer {
    let logs = InMemoryLogStorage::new();
    logs.seed(attempt_id(), "first try failed\n");
    logs.seed(retry_attempt_id(), "second try built\n");
    TestState::new()
        .web_db(
            db.append_query_results([failed_and_retried_attempts()])
                .append_query_results([vec![gradient_entity::dispatched_job::Model {
                    id: DispatchedJobId::new(Uuid::from_u128(0xd2)),
                    worker_id: "worker-2".into(),
                    ..Default::default()
                }]])
                .into_connection(),
        )
        .log_storage(Arc::new(logs))
        .server()
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

#[tokio::test]
async fn get_build_attempts_lists_both_attempts_oldest_first_with_outcome_and_worker_id() {
    let res = server_with_failed_and_retried_attempts(
        mock_database_connection_with_build_access_check_rows(BuildStatus::Completed),
    )
    .get(&format!("/api/v1/builds/{}/attempts", build_job_id()))
    .await;

    res.assert_status_ok();
    let attempts: Value = res.json();
    let summary: Vec<(u64, &str, Option<&str>, Option<&str>)> = attempts["message"]
        .as_array()
        .expect("a list of attempts")
        .iter()
        .map(|a| {
            (
                a["number"].as_u64().unwrap_or_default(),
                a["outcome"].as_str().unwrap_or_default(),
                a["reason"].as_str(),
                a["worker"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (1, "Failed", Some("BuilderNonzero"), None),
            (2, "Built", None, Some("worker-2")),
        ]
    );
}

#[tokio::test]
async fn get_build_log_returns_requested_attempt_log_and_latest_attempt_log_without_attempt_parameter()
 {
    let path = format!("/api/v1/builds/{}/log", build_job_id());

    let first = server_with_failed_and_retried_attempts(
        mock_database_connection_with_build_access_check_rows(BuildStatus::Completed),
    )
    .get(&format!("{path}?attempt=1"))
    .add_header("accept", "text/plain")
    .await;
    let latest = TestState::new()
        .web_db(
            mock_database_connection_with_build_access_check_rows(BuildStatus::Completed)
                .append_query_results([vec![failed_and_retried_attempts().remove(1)]])
                .into_connection(),
        )
        .log_storage(Arc::new({
            let logs = InMemoryLogStorage::new();
            logs.seed(attempt_id(), "first try failed\n");
            logs.seed(retry_attempt_id(), "second try built\n");
            logs
        }))
        .server()
        .get(&path)
        .add_header("accept", "text/plain")
        .await;

    first.assert_status_ok();
    latest.assert_status_ok();
    assert_eq!(
        (first.text(), latest.text()),
        (
            "first try failed\n".to_owned(),
            "second try built\n".to_owned()
        )
    );
}

#[tokio::test]
async fn get_build_log_with_missing_attempt_returns_404() {
    let res = server_with_failed_and_retried_attempts(
        mock_database_connection_with_build_access_check_rows(BuildStatus::Completed),
    )
    .get(&format!("/api/v1/builds/{}/log?attempt=3", build_job_id()))
    .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}
