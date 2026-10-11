/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::multipart::{MultipartForm, Part};
use gradient_entity::ids::*;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{
    build_request_commit, build_request_task, project_member, queued_evaluation, write_role,
};
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::Value;

fn with_project_access(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![gradient_fixtures::rows::project()]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
}

fn source_url() -> String {
    "/api/v1/build-requests/source?project=test-project".to_string()
}

#[tokio::test]
async fn source_upload_creates_queued_eval() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let task_id = TaskId::now_v7();
    let task_model = build_request_task(task_id);
    let commit_model = build_request_commit();
    let eval_model = queued_evaluation(task_id, commit_model.id);

    let db = with_project_access(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .append_query_results([vec![task_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![commit_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![eval_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()]);

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new()
        .add_part("nar", Part::bytes(b"a pretend source nar".to_vec()))
        .add_text("target", "packages.x86_64-linux.hello")
        .add_text("system", "x86_64-linux");

    let res = server
        .post(&source_url())
        .add_header("authorization", format!("Bearer {}", token))
        .multipart(form)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(
        body["message"]["evaluation"].as_str().unwrap(),
        eval_model.id.to_string()
    );
    assert!(body["message"]["cache"].is_null());
}

#[tokio::test]
async fn source_upload_missing_nar_is_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_access(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());

    let form = MultipartForm::new().add_text("target", "x");

    let res = server
        .post(&source_url())
        .add_header("authorization", format!("Bearer {}", token))
        .multipart(form)
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}
