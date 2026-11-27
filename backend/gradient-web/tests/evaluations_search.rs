/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::{commit, evaluation, ids::*};
use gradient_fixtures::ids::{commit_id, task_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{public_project, task};
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
        ..Default::default()
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
        .append_query_results([vec![task()]])
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
    let server = make_test_server(
        with_summary_rollups(db)
            .append_query_results([Vec::<commit::Model>::new()])
            .into_connection(),
    );

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
async fn test_list_task_evaluations_before_cursor_returns_older_evaluations_with_id_tiebreak() {
    use gradient_fixtures::shape::shape;

    let mut ids = [(); 5].map(|_| EvaluationId::now_v7());
    ids.sort();
    let [
        newer_lower_id,
        same_time_lower_id,
        cursor,
        same_time_higher_id,
        older_higher_id,
    ] = ids;
    let mut declared = shape()
        .evaluation(cursor)
        .evaluation_status(EvaluationStatus::Completed);
    for id in [
        newer_lower_id,
        same_time_lower_id,
        same_time_higher_id,
        older_higher_id,
    ] {
        declared = declared
            .evaluation(id)
            .same_task_as(cursor)
            .evaluation_status(EvaluationStatus::Completed);
    }
    let test_db = declared.into_database().await;
    let db = test_db.connection();
    for (id, day) in [
        (newer_lower_id, 3),
        (same_time_lower_id, 2),
        (cursor, 2),
        (same_time_higher_id, 2),
        (older_higher_id, 1),
    ] {
        test_db
            .execute(&format!(
                "UPDATE evaluation SET created_at = '2026-01-0{day}' WHERE id = '{id}'"
            ))
            .await;
    }
    let task = test_db
        .find::<gradient_entity::task::Entity>(test_db.task_of(cursor).await)
        .await
        .expect("task");
    let project = test_db.project_of(cursor).await;
    test_db
        .execute(&format!(
            "UPDATE project SET public = true WHERE id = '{project}'"
        ))
        .await;
    let project = test_db
        .find::<gradient_entity::project::Entity>(project)
        .await
        .expect("project");
    let session_id = SessionId::now_v7();
    gradient_db::rows::insert::<gradient_entity::session::Entity, _>(
        &db,
        gradient_fixtures::rows::live_session(session_id),
    )
    .await
    .expect("session");
    let server = make_test_server(db);

    let res = server
        .get(&format!(
            "/api/v1/tasks/{}/{}/evaluations?before={cursor}&limit=10",
            project.name, task.name
        ))
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let mut paged: Vec<String> = body["message"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    paged.sort();
    assert_eq!(
        paged,
        [same_time_lower_id, older_higher_id].map(|id| id.to_string())
    );
    drop(server);
    test_db.finish().await;
}
