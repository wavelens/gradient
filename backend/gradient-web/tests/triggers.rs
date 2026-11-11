/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::{ids::*, task, task_trigger};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::{all_statements, mock_db};
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token, signed_in, with_auth};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::{ConcurrencyPolicy, SessionId, TriggerType};
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::Value;
use uuid::Uuid;

fn trigger_id() -> TaskTriggerId {
    TaskTriggerId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap())
}

fn polling_trigger_row() -> task_trigger::Model {
    task_trigger::Model {
        id: trigger_id(),
        task: task_id(),
        config: serde_json::json!({"interval_secs": 60}),
        active: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn reporter_push_trigger_row() -> task_trigger::Model {
    task_trigger::Model {
        id: trigger_id(),
        task: task_id(),
        trigger_type: TriggerType::ReporterPush,
        config: serde_json::json!({
            "branches": ["main"],
            "tags": [],
            "releases_only": false,
        }),
        active: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn with_task_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

fn with_task_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

const BASE_URL: &str = "/api/v1/tasks/test-project/test-task/triggers";

#[tokio::test]
async fn list_triggers_returns_rows() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let items = body["message"].as_array().expect("message is array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["type"], "polling");
    assert_eq!(items[0]["active"], true);
}

#[tokio::test]
async fn get_trigger_returns_row() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["type"], "polling");
    assert_eq!(body["message"]["id"], tid.to_string());
}

#[tokio::test]
async fn get_trigger_not_found_returns_404() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = TaskTriggerId::now_v7();

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_trigger::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn create_polling_trigger_valid() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "config": {"type": "polling", "interval_secs": 60}
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["type"], "polling");
}

#[tokio::test]
async fn create_polling_trigger_interval_too_small_returns_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "config": {"type": "polling", "interval_secs": 5}
        }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    let msg = body["message"].as_str().unwrap();
    assert!(
        msg.contains("interval_secs"),
        "expected interval message, got: {msg}"
    );
}

#[tokio::test]
async fn create_invalid_cron_returns_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "config": {"type": "time", "cron": "not a cron"}
        }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn patch_trigger_updates_fields() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let updated = polling_trigger_row();

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]])
        .append_query_results([vec![updated]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"active": false}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["type"], "polling");
}

#[tokio::test]
async fn patch_trigger_config_type_change() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let updated = task_trigger::Model {
        trigger_type: TriggerType::Time,
        config: serde_json::json!({"cron": "0 0 2 * * *"}),
        ..polling_trigger_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]])
        .append_query_results([vec![updated]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "config": {"type": "time", "cron": "0 0 2 * * *"}
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["type"], "time");
}

#[tokio::test]
async fn delete_trigger_removes_row() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![polling_trigger_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["deleted"], true);
}

#[tokio::test]
async fn delete_trigger_not_found_returns_404() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = TaskTriggerId::now_v7();

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_trigger::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn delete_default_trigger_returns_409() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let default_trigger = task_trigger::Model {
        is_default: true,
        ..polling_trigger_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![default_trigger]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!("{}/{}", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn fire_now_on_inactive_trigger_returns_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let tid = trigger_id();

    let inactive_trigger = task_trigger::Model {
        active: false,
        ..polling_trigger_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![inactive_trigger]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(&format!("{}/{}/test", BASE_URL, tid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("inactive"),
        "expected inactive mention, got: {}",
        body["message"]
    );
}

// `fire_now` is not integration-tested here because `head_commit` is making real git network
// requests.

#[tokio::test]
async fn create_task_seeds_default_polling_trigger() {
    let (db, auth) = signed_in();

    let created_task = task::Model {
        id: task_id(),
        project: project_id(),
        name: "new-task".into(),
        active: true,
        display_name: "New Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::Skip,
        ..Default::default()
    };

    let seeded_trigger = task_trigger::Model {
        id: trigger_id(),
        task: task_id(),
        config: serde_json::json!({"interval_secs": 300}),
        active: true,
        is_default: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([Vec::<task::Model>::new()])
        .append_query_results([vec![created_task]])
        .append_query_results([vec![seeded_trigger]]);

    let conn = db.into_connection();
    let server = make_test_server(conn.clone());
    let res = server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", auth)
        .json(&serde_json::json!({
            "name": "new-task",
            "display_name": "New Task",
            "description": "",
            "repository": "https://github.com/test/repo",
            "wildcard": "*"
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["id"], task_id().to_string());
    drop(server);
    let task_and_trigger_in_one_transaction = conn.into_transaction_log().iter().any(|t| {
        let sql: Vec<&str> = t.statements().iter().map(|s| s.sql.as_str()).collect();
        sql.iter().any(|s| s.starts_with(r#"INSERT INTO "task" "#))
            && sql
                .iter()
                .any(|s| s.starts_with(r#"INSERT INTO "task_trigger""#))
    });
    assert!(task_and_trigger_in_one_transaction);
}

#[tokio::test]
async fn patch_task_concurrency_to_skip() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![task()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch("/api/v1/tasks/test-project/test-task")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"concurrency": "skip"}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
}

#[tokio::test]
async fn changing_the_repository_clears_the_fetch_status() {
    let session_id = SessionId::now_v7();
    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![task()]])
        .into_connection();

    let res = make_test_server(db.clone())
        .patch("/api/v1/tasks/test-project/test-task")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({"repository": "https://github.com/test/other"}))
        .await;

    res.assert_status_ok();
    let update = all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.starts_with(r#"UPDATE "task""#))
        .expect("the task row is updated");
    for column in [
        "last_fetch_at",
        "last_fetch_ok_at",
        "last_fetch_error",
        "force_evaluation",
    ] {
        assert!(update.sql.contains(column), "{column} in {}", update.sql);
    }
}

#[tokio::test]
async fn a_description_only_task_patch_does_not_force_an_evaluation() {
    let session_id = SessionId::now_v7();
    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![task()]])
        .into_connection();

    let res = make_test_server(db.clone())
        .patch("/api/v1/tasks/test-project/test-task")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({"description": "Only the text can change."}))
        .await;

    res.assert_status_ok();
    let update = all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.starts_with(r#"UPDATE "task""#))
        .expect("the task row is updated");
    let set_clause = update.sql.split(" WHERE ").next().unwrap_or_default();
    assert!(!set_clause.contains("force_evaluation"), "{}", update.sql);
}

#[tokio::test]
async fn list_triggers_returns_push_trigger_without_git_host_connection_or_integration_field() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![reporter_push_trigger_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let item = &body["message"][0];
    assert_eq!(item["type"], "reporter_push");
    assert!(item.get("integration").is_none(), "{item}");
}
