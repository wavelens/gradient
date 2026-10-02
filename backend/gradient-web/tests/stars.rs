/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{project, task};
use gradient_test_support::fixtures::{self, user};
use gradient_test_support::web::{
    live_session, make_test_server, make_test_server_with_worker_db, make_token,
};
use gradient_types::{MProject, SessionId};
use sea_orm::{
    DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Statement, Value,
};
use serde_json::Value as Json;
use std::collections::BTreeMap;

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn public_project() -> project::Model {
    project::Model {
        public: true,
        ..fixtures::project()
    }
}

fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

fn statements(db: DatabaseConnection) -> Vec<Statement> {
    db.into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .collect()
}

fn ran(db: DatabaseConnection, prefix: &str) -> bool {
    statements(db).iter().any(|s| s.sql.starts_with(prefix))
}

fn exec_ok() -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    }
}

#[tokio::test]
async fn starring_an_unknown_project_is_not_found() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([Vec::<MProject>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .put("/api/v1/user/stars/projects/nope")
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn starring_twice_is_fine() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![public_project()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }]);
    let db = db.into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .put("/api/v1/user/stars/projects/test-project")
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"], true);
    assert!(ran(db, "INSERT INTO user_project_star"));
}

#[tokio::test]
async fn unstarring_twice_is_fine() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![public_project()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }]);
    let db = db.into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .delete("/api/v1/user/stars/projects/test-project")
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"], false);
    assert!(ran(db, "DELETE FROM user_project_star"));
}

#[tokio::test]
async fn stars_list_groups_by_kind() {
    let session_id = SessionId::now_v7();
    let row = |kind: &str, project: Option<&str>, name: &str| {
        BTreeMap::from([
            ("kind", Value::from(kind)),
            ("project", Value::String(project.map(str::to_string))),
            ("name", Value::from(name)),
            ("display_name", Value::from(name.to_uppercase())),
            ("starred", Value::from(true)),
        ])
    };
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![
            row("cache", None, "main"),
            row("project", None, "infra"),
            row("task", Some("infra"), "hosts"),
        ]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/user/stars")
        .add_header("Authorization", bearer(session_id))
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
    let session_id = SessionId::now_v7();
    let task = task::Model {
        id: fixtures::task_id(),
        project: fixtures::project_id(),
        name: "test-task".into(),
        ..Default::default()
    };
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![public_project()]])
        .append_query_results([vec![task]])
        .append_exec_results([exec_ok()])
        .into_connection();
    let worker_db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([exec_ok()])
        .into_connection();
    let server = make_test_server_with_worker_db(db.clone(), worker_db.clone());

    let res = server
        .put("/api/v1/user/stars/tasks/test-project/test-task")
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    assert!(ran(db, "INSERT INTO user_task_star"));
    let delivery = statements(worker_db)
        .into_iter()
        .find(|s| s.sql.contains("INSERT INTO pending_delivery"))
        .expect("event row");
    let payload = format!("{:?}", delivery.values);
    assert!(payload.contains("TaskStar"), "{payload}");
}
