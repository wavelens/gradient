/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::ids::*;
use gradient_entity::task_flake_input_override;
use gradient_fixtures::ids::task_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::Value;
use uuid::Uuid;

fn override_id() -> FlakeInputOverrideId {
    FlakeInputOverrideId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000077").unwrap())
}

fn managed_task_row() -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        managed: true,
        ..task()
    }
}

fn nixpkgs_override_row() -> task_flake_input_override::Model {
    task_flake_input_override::Model {
        id: override_id(),
        task: task_id(),
        input_name: "nixpkgs".into(),
        url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn with_task_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
}

fn with_task_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

fn with_managed_task_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![managed_task_row()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

const BASE_URL: &str = "/api/v1/tasks/test-project/test-task/flake-inputs";

#[tokio::test]
async fn list_empty_returns_empty() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_flake_input_override::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn create_then_list_returns_one() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_flake_input_override::Model>::new()])
        .append_query_results([vec![nixpkgs_override_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "input_name": "nixpkgs",
            "url": "github:NixOS/nixpkgs/nixos-unstable"
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["input_name"], "nixpkgs");
    assert_eq!(
        body["message"]["url"],
        "github:NixOS/nixpkgs/nixos-unstable"
    );
}

#[tokio::test]
async fn create_with_null_url_keep_url_mode() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let null_url_row = task_flake_input_override::Model {
        id: override_id(),
        task: task_id(),
        input_name: "utils".into(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_flake_input_override::Model>::new()])
        .append_query_results([vec![null_url_row]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"input_name": "utils", "url": null}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["input_name"], "utils");
    assert!(body["message"]["url"].is_null());
}

#[tokio::test]
async fn create_with_glob_input_name() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let glob_row = task_flake_input_override::Model {
        id: override_id(),
        task: task_id(),
        input_name: "nixpkgs*".into(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_flake_input_override::Model>::new()])
        .append_query_results([vec![glob_row]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"input_name": "nixpkgs*", "url": null}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["input_name"], "nixpkgs*");
}

#[tokio::test]
async fn create_duplicate_input_name_rejects_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![nixpkgs_override_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "input_name": "nixpkgs",
            "url": "github:NixOS/nixpkgs/nixos-unstable"
        }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn create_invalid_input_name_rejects_400() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_task_edit(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"input_name": "bad name!", "url": "x"}))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("input_name"),
        "expected input_name in message, got: {}",
        body["message"]
    );
}

#[tokio::test]
async fn patch_updates_url_and_returns_new_row() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let oid = override_id();

    let updated = task_flake_input_override::Model {
        url: Some("github:NixOS/nixpkgs/nixos-24.05".into()),
        ..nixpkgs_override_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_query_results([vec![updated]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!("{}/{}", BASE_URL, oid))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"url": "github:NixOS/nixpkgs/nixos-24.05"}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["url"], "github:NixOS/nixpkgs/nixos-24.05");
}

#[tokio::test]
async fn patch_url_to_null_sets_keep_url() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let oid = override_id();

    let updated = task_flake_input_override::Model {
        url: None,
        ..nixpkgs_override_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_query_results([vec![updated]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!("{}/{}", BASE_URL, oid))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"url": null}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert!(body["message"]["url"].is_null());
}

#[tokio::test]
async fn patch_omitting_url_does_not_change_it() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let oid = override_id();

    let updated = task_flake_input_override::Model {
        input_name: "renamed".into(),
        url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
        ..nixpkgs_override_row()
    };

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_query_results([Vec::<task_flake_input_override::Model>::new()])
        .append_query_results([vec![updated]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!("{}/{}", BASE_URL, oid))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"input_name": "renamed"}))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["input_name"], "renamed");
    assert_eq!(
        body["message"]["url"],
        "github:NixOS/nixpkgs/nixos-unstable"
    );
}

#[tokio::test]
async fn delete_removes_the_row() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let oid = override_id();

    let db = with_task_edit(with_auth(mock_db(), session_id))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!("{}/{}", BASE_URL, oid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["deleted"], true);
}

#[tokio::test]
async fn get_not_found_returns_404() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let oid = FlakeInputOverrideId::now_v7();

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<task_flake_input_override::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(&format!("{}/{}", BASE_URL, oid))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn list_sorted_by_input_name() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let alpha = task_flake_input_override::Model {
        id: FlakeInputOverrideId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        ),
        task: task_id(),
        input_name: "alpha".into(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };
    let zebra = task_flake_input_override::Model {
        id: FlakeInputOverrideId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
        ),
        task: task_id(),
        input_name: "zebra".into(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };

    let db = with_task_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![alpha, zebra]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let items = body["message"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["input_name"], "alpha");
    assert_eq!(items[1]["input_name"], "zebra");
}

#[tokio::test]
async fn managed_task_rejects_mutations_403() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_managed_task_edit(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
    let res = server
        .post(BASE_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "input_name": "nixpkgs",
            "url": "github:NixOS/nixpkgs/nixos-unstable"
        }))
        .await;

    res.assert_status_forbidden();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}
