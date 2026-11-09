/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, superuser, user};
use gradient_test_support::web::{make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::MockDatabase;
use serde_json::Value;

fn with_user(
    db: MockDatabase,
    session_id: SessionId,
    caller: gradient_entity::user::Model,
) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![caller]])
}

#[tokio::test]
async fn assign_decisions_rejects_non_superuser() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(mock_db(), session_id, user());

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/jobs/decisions")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn assign_decisions_superuser_returns_empty_ring() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(mock_db(), session_id, superuser());

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/jobs/decisions")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn pending_jobs_of_an_idle_scheduler_are_empty() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(mock_db(), session_id, superuser())
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/jobs/pending")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"],
        serde_json::json!({ "jobs": [], "other_pending": 0 })
    );
}

#[tokio::test]
async fn scoring_summary_rejects_non_superuser() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(mock_db(), session_id, user());

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/scoring/summary")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn a_pending_page_after_assignment_names_the_latest_assignment() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let (job, project) = (uuid::Uuid::now_v7(), gradient_fixtures::ids::project_id());
    let task = gradient_entity::task::Model {
        id: gradient_fixtures::ids::task_id(),
        project,
        name: "nightly".into(),
        ..Default::default()
    };
    let evaluation = gradient_entity::evaluation::Model {
        id: gradient_types::EvaluationId::now_v7(),
        task: Some(task.id),
        ..Default::default()
    };
    let db = with_user(mock_db(), session_id, superuser())
        // Mock databases decode tuple columns by position. Row maps sort keys, so prefixes keep the selection order.
        .append_query_results([vec![std::collections::BTreeMap::from([
            ("a_id", sea_orm::Value::from(job)),
            ("b_evaluation_id", evaluation.id.into_inner().into()),
            ("c_project", project.into_inner().into()),
        ])]])
        .append_query_results([vec![evaluation.clone()]])
        .append_query_results([vec![task]])
        .append_query_results([vec![gradient_entity::project::Model {
            id: project,
            name: "acme".into(),
            ..Default::default()
        }]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(&format!(
            "/api/v1/board/jobs/pending/{}",
            uuid::Uuid::now_v7()
        ))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"],
        serde_json::json!({
            "dispatched_job_id": job,
            "evaluation_id": evaluation.id,
            "project_name": "acme",
        })
    );
}
