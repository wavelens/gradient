/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use chrono::Utc;
use gradient_entity::ids::*;
use gradient_fixtures::rows::{
    build_request_commit, build_request_task, project_member, queued_evaluation, upload_session,
    write_role,
};
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::MockExecResult;
use serde_json::{Value, json};

fn dispatch_url(session: UploadSessionId) -> String {
    format!("/api/v1/build-requests/{}/dispatch", session)
}

#[tokio::test]
async fn rejects_already_dispatched_session() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let db = db
        .append_query_results([vec![upload_session(upload, vec![], true, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejects_expired_session() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let db = db
        .append_query_results([vec![upload_session(upload, vec![], false, true)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn rejects_session_with_missing_blobs() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let missing = vec!["a".repeat(64)];

    let db = db
        .append_query_results([vec![upload_session(upload, missing, false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejects_session_not_found() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let db = db.append_query_results([Vec::<gradient_entity::upload_session::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn happy_path_creates_task_commit_and_evaluation() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let task_id = TaskId::now_v7();
    let task_model = build_request_task(task_id);
    let commit_model = build_request_commit();
    let eval_model = queued_evaluation(task_id, commit_model.id);

    let updated = gradient_entity::upload_session::Model {
        dispatched_at: Some(Utc::now().naive_utc()),
        ..upload_session(upload, vec![], false, false)
    };

    let db = db
        .append_query_results([vec![upload_session(upload, vec![], false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
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
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
        .append_query_results([vec![updated]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(
        body["message"]["task"].as_str().unwrap(),
        task_id.to_string()
    );
    assert_eq!(
        body["message"]["commit"].as_str().unwrap(),
        commit_model.id.to_string()
    );
    assert_eq!(
        body["message"]["evaluation"].as_str().unwrap(),
        eval_model.id.to_string()
    );
    assert!(
        body["message"].as_object().unwrap().contains_key("cache"),
        "DispatchResponse must carry a `cache` field"
    );
    assert!(body["message"]["cache"].is_null());
}

#[tokio::test]
async fn rejects_local_input_override() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let db = db
        .append_query_results([vec![upload_session(upload, vec![], false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({
            "input_overrides": [{ "input_name": "nixpkgs", "url": "/home/u/np" }]
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn happy_path_reuses_existing_build_request_task() {
    let (db, auth) = signed_in();
    let upload = UploadSessionId::now_v7();

    let task_id = TaskId::now_v7();
    let task_model = build_request_task(task_id);
    let commit_model = build_request_commit();
    let eval_model = queued_evaluation(task_id, commit_model.id);

    let updated = gradient_entity::upload_session::Model {
        dispatched_at: Some(Utc::now().naive_utc()),
        ..upload_session(upload, vec![], false, false)
    };

    let db = db
        .append_query_results([vec![upload_session(upload, vec![], false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
        .append_query_results([vec![task_model.clone()]])
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
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
        .append_query_results([vec![updated]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"]["task"].as_str().unwrap(),
        task_id.to_string()
    );
}

const JOB_TOKEN: &str =
    "Bearer GRJ_0123456789012345678901234567890123456789012345678901234567890123";

fn job_id() -> ExecJobId {
    ExecJobId::new(uuid::Uuid::from_u128(0x7a))
}

fn job_token_auth() -> sea_orm::MockDatabase {
    use gradient_fixtures::ids::{project_id, task_id, user_id};
    use sea_orm::Value as Column;

    let resolved = std::collections::BTreeMap::from([
        ("job", Column::from(job_id().into_inner())),
        ("attempt", Column::from(uuid::Uuid::from_u128(0xa1))),
        ("project", Column::from(project_id().into_inner())),
        ("task", Column::from(task_id().into_inner())),
        ("project_name", Column::from("test-project")),
        ("task_name", Column::from("test-task")),
        ("acting_user", Column::from(user_id().into_inner())),
    ]);
    gradient_fixtures::mock::mock_db()
        .append_query_results([vec![resolved]])
        .append_query_results([vec![gradient_fixtures::rows::user()]])
}

async fn dispatched_parent(db: sea_orm::MockDatabase, auth: &str) -> sea_orm::Value {
    let upload = UploadSessionId::now_v7();
    let task_id = TaskId::now_v7();
    let commit_model = build_request_commit();
    let updated = gradient_entity::upload_session::Model {
        dispatched_at: Some(Utc::now().naive_utc()),
        ..upload_session(upload, vec![], false, false)
    };
    let executed = || MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let db = db
        .append_query_results([vec![upload_session(upload, vec![], false, false)]])
        .append_query_results([vec![project_member(BASE_ROLE_WRITE_ID)]])
        .append_query_results([vec![write_role()]])
        .append_query_results([vec![build_request_task(task_id)]])
        .append_query_results([vec![commit_model.clone()]])
        .append_exec_results([executed()])
        .append_query_results([vec![queued_evaluation(task_id, commit_model.id)]])
        .append_exec_results([executed()])
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
        .append_query_results([vec![updated]])
        .append_exec_results([executed()])
        .into_connection();

    let res = make_test_server(db.clone())
        .post(&dispatch_url(upload))
        .add_header("authorization", auth)
        .json(&json!({}))
        .await;
    res.assert_status_ok();

    let log = gradient_fixtures::mock::raw_statements(db.into_transaction_log());
    let insert = log
        .iter()
        .find(|s| s.sql.starts_with(r#"INSERT INTO "evaluation""#))
        .expect("the evaluation is inserted");
    let columns: Vec<&str> = insert.sql[insert.sql.find('(').expect("column list") + 1
        ..insert.sql.find(')').expect("column list end")]
        .split(", ")
        .collect();
    let at = columns
        .iter()
        .position(|c| *c == r#""parent_exec_job""#)
        .expect("the insert names parent_exec_job");
    insert.values.as_ref().expect("bound values").0[at].clone()
}

#[tokio::test]
async fn a_build_request_from_a_job_names_its_parent_job() {
    let from_job = dispatched_parent(job_token_auth(), JOB_TOKEN).await;
    assert_eq!(from_job, sea_orm::Value::Uuid(Some(job_id().into_inner())));

    let (db, auth) = signed_in();
    let from_user = dispatched_parent(db, &auth).await;
    assert_eq!(from_user, sea_orm::Value::Uuid(None));
}
