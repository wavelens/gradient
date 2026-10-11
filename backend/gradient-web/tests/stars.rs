/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::task;
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db, ran};
use gradient_fixtures::rows::public_project;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::MProject;
use sea_orm::{MockExecResult, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;

#[tokio::test]
async fn starring_an_unknown_project_is_not_found() {
    let (db, auth) = signed_in();
    let db = db.append_query_results([Vec::<MProject>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .put("/api/v1/user/stars/projects/nope")
        .add_header("Authorization", auth)
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn starring_twice_is_fine() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![public_project()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }]);
    let db = db.into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .put("/api/v1/user/stars/projects/test-project")
        .add_header("Authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"], true);
    assert!(ran(db, "INSERT INTO user_project_star"));
}

#[tokio::test]
async fn unstarring_twice_is_fine() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![public_project()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }]);
    let db = db.into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .delete("/api/v1/user/stars/projects/test-project")
        .add_header("Authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"], false);
    assert!(ran(db, "DELETE FROM user_project_star"));
}

#[tokio::test]
async fn stars_list_groups_by_kind() {
    let (db, auth) = signed_in();
    let row = |kind: &str, project: Option<&str>, name: &str| {
        BTreeMap::from([
            ("kind", Value::from(kind)),
            ("project", Value::String(project.map(str::to_string))),
            ("name", Value::from(name)),
            ("display_name", Value::from(name.to_uppercase())),
            ("starred", Value::from(true)),
        ])
    };
    let db = db.append_query_results([vec![
        row("cache", None, "main"),
        row("project", None, "infra"),
        row("task", Some("infra"), "hosts"),
    ]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/user/stars")
        .add_header("Authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["message"]["projects"], serde_json::json!(["infra"]));
    assert_eq!(
        body["message"]["tasks"],
        serde_json::json!([{ "project": "infra", "task": "hosts" }])
    );
    assert_eq!(body["message"]["caches"], serde_json::json!(["main"]));
}

#[tokio::test]
async fn starring_a_task_records_a_task_star_event() {
    let (db, auth) = signed_in();
    let task = task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        ..Default::default()
    };
    let db = db
        .append_query_results([vec![public_project()]])
        .append_query_results([vec![task]])
        .append_exec_results([exec_ok()])
        .into_connection();
    let worker_db = mock_db().append_exec_results([exec_ok()]).into_connection();
    let server = TestState::new()
        .web_db(db.clone())
        .worker_db(worker_db.clone())
        .server();

    let res = server
        .put("/api/v1/user/stars/tasks/test-project/test-task")
        .add_header("Authorization", auth)
        .await;

    res.assert_status_ok();
    assert!(ran(db, "INSERT INTO user_task_star"));
    let delivery = all_statements(worker_db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.contains("INSERT INTO pending_delivery"))
        .expect("event row");
    let payload = format!("{:?}", delivery.values);
    assert!(payload.contains("TaskStar"), "{payload}");
}
