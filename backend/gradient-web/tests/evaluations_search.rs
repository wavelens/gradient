/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::{commit, evaluation, ids::*};
use gradient_fixtures::ids::{commit_id, task_id};
use gradient_fixtures::mock::{all_statements, mock_db};
use gradient_fixtures::rows::{public_project, task_without_cache_signing};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;
use sea_orm::MockDatabase;
use serde_json::Value;

const COMMIT_HEX: &str = "1111111111111111111111111111111111111111";

fn commit_row() -> commit::Model {
    commit::Model {
        id: commit_id(),
        message: "test commit".into(),
        hash: vec![0x11; 20],
        author: None,
        author_name: "Tester".into(),
    }
}

fn eval_row(id: EvaluationId, wildcard: &str, status: EvaluationStatus) -> evaluation::Model {
    evaluation::Model {
        id,
        task: Some(task_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: wildcard.into(),
        status,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn with_task(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![public_project()]])
        .append_query_results([vec![task_without_cache_signing()]])
}

fn with_summary_rollups(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<commit::Model>::new()])
        .append_query_results([Vec::<commit::Model>::new()])
        .append_query_results([Vec::<commit::Model>::new()])
        .append_query_results([Vec::<commit::Model>::new()])
}

fn base_db(session_id: SessionId) -> MockDatabase {
    with_task(with_auth(mock_db(), session_id))
}

const URL: &str = "/api/v1/tasks/test-project/test-task/evaluations";

#[tokio::test]
async fn rejects_commit_that_is_not_a_full_hash() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(base_db(session_id).into_connection());

    let res = server
        .get(&format!("{URL}?commit=1111"))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("40-character"),
        "unexpected message: {}",
        body["message"]
    );
}

#[tokio::test]
async fn rejects_unknown_status() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(base_db(session_id).into_connection());

    let res = server
        .get(&format!("{URL}?status=Exploded"))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("Exploded"),
        "unexpected message: {}",
        body["message"]
    );
}

#[tokio::test]
async fn rejects_wildcard_syntax_in_attr() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(base_db(session_id).into_connection());

    let res = server
        .get(&format!("{URL}?attr=packages.*.hello"))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("concrete"),
        "unexpected message: {}",
        body["message"]
    );
}

#[tokio::test]
async fn unknown_commit_returns_empty_list() {
    let session_id = SessionId::now_v7();
    let db = base_db(session_id).append_query_results([Vec::<commit::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("{URL}?commit={COMMIT_HEX}"))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn attr_filter_matches_queued_evaluation() {
    let session_id = SessionId::now_v7();
    let queued = EvaluationId::now_v7();

    let db = base_db(session_id)
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![
            eval_row(queued, "packages.*.*", EvaluationStatus::Queued),
            eval_row(
                EvaluationId::now_v7(),
                "checks.*.*",
                EvaluationStatus::Completed,
            ),
        ]])
        .append_query_results([vec![commit_row()]]);
    let server = make_test_server(with_summary_rollups(db).into_connection());

    let res = server
        .get(&format!(
            "{URL}?commit={COMMIT_HEX}&attr=packages.x86_64-linux.hello"
        ))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let rows = body["message"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "checks.*.* must be filtered out: {body}");
    assert_eq!(rows[0]["id"], queued.to_string());
    assert_eq!(rows[0]["status"], "Queued");
    assert_eq!(rows[0]["wildcard"], "packages.*.*");
    assert_eq!(rows[0]["commit"], COMMIT_HEX);
}

#[tokio::test]
async fn before_an_evaluation_of_another_task_is_not_found() {
    let session_id = SessionId::now_v7();
    let db = base_db(session_id).append_query_results([Vec::<evaluation::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("{URL}?before={}", EvaluationId::now_v7()))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn before_pages_to_evaluations_older_than_the_cursor() {
    let session_id = SessionId::now_v7();
    let cursor = eval_row(EvaluationId::now_v7(), "*", EvaluationStatus::Queued);
    let db = base_db(session_id)
        .append_query_results([vec![cursor.clone()]])
        .append_query_results([Vec::<evaluation::Model>::new()])
        .into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .get(&format!("{URL}?before={}&limit=10", cursor.id))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    drop(server);
    let page = all_statements(db.into_transaction_log())
        .pop()
        .expect("page query");
    assert!(
        page.sql.contains(r#""evaluation"."created_at" < $"#),
        "page must stop at the cursor: {}",
        page.sql
    );
    assert!(
        page.sql.contains(r#""evaluation"."id" < $"#),
        "evaluations created at the same time must page by id: {}",
        page.sql
    );
}
