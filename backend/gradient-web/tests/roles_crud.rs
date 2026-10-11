/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! An `INSERT ... RETURNING` must get the row via `append_query_results` and an
//! `append_exec_results` with `rows_affected: 1`. SeaORM is otherwise treating the insert as a
//! no-op.

use gradient_entity::{ids::*, project_user, role};
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows::{project, project_member};
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::permissions::{Permission, admin_mask, view_mask, write_mask};
use sea_orm::MockExecResult;
use serde_json::{Value, json};
use uuid::Uuid;

fn admin_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: admin_mask(),
        ..Default::default()
    }
}

fn view_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_VIEW_ID,
        name: "View".into(),
        permission: view_mask(),
        ..Default::default()
    }
}

fn write_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "Write".into(),
        permission: write_mask(),
        ..Default::default()
    }
}

fn custom_role_row(id: RoleId, name: &str, permission: i64) -> role::Model {
    role::Model {
        id,
        name: name.into(),
        project: Some(project_id()),
        permission,
        ..Default::default()
    }
}

#[tokio::test]
async fn list_roles_returns_builtins_plus_custom() {
    let (db, auth) = signed_in();
    let custom_id = RoleId::now_v7();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![
            admin_role_row(),
            write_role_row(),
            view_role_row(),
            custom_role_row(custom_id, "releaser", Permission::TriggerEvaluation.bit()),
        ]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/projects/test-project/roles")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let roles = body["message"]["roles"].as_array().expect("roles array");
    assert_eq!(roles.len(), 4);

    let admin = roles
        .iter()
        .find(|r| r["name"] == "Admin")
        .expect("admin role");
    assert_eq!(admin["builtin"], true);
    assert!(admin["permissions"].as_array().unwrap().len() >= 13);

    let releaser = roles
        .iter()
        .find(|r| r["name"] == "releaser")
        .expect("custom role");
    assert_eq!(releaser["builtin"], false);
    assert_eq!(releaser["permissions"], json!(["triggerEvaluation"]));

    let perms = body["message"]["available_permissions"]
        .as_array()
        .expect("available_permissions array");
    assert!(perms.iter().any(|p| p["id"] == "manageRoles"));
}

#[tokio::test]
async fn create_role_persists_permission_bitmask() {
    let (db, auth) = signed_in();
    let inserted = custom_role_row(
        RoleId::now_v7(),
        "releaser",
        Permission::TriggerEvaluation.bit() | Permission::ViewProject.bit(),
    );

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results::<role::Model, _, _>([Vec::<role::Model>::new()])
        .append_query_results([vec![inserted.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/roles")
        .add_header("authorization", auth)
        .json(&json!({
            "name": "releaser",
            "permissions": ["triggerEvaluation", "viewProject"],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["name"], "releaser");
    assert_eq!(body["message"]["builtin"], false);
    let perms = body["message"]["permissions"].as_array().unwrap();
    assert!(perms.iter().any(|p| p == "triggerEvaluation"));
    assert!(perms.iter().any(|p| p == "viewProject"));
}

#[tokio::test]
async fn create_role_rejects_unknown_permission() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/roles")
        .add_header("authorization", auth)
        .json(&json!({
            "name": "releaser",
            "permissions": ["banana"],
        }))
        .await;

    res.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(body["message"].as_str().unwrap().contains("banana"));
}

#[tokio::test]
async fn create_role_rejects_view_role_caller() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![view_role_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/roles")
        .add_header("authorization", auth)
        .json(&json!({
            "name": "releaser",
            "permissions": ["triggerEvaluation"],
        }))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_role_rejects_duplicate_name() {
    let (db, auth) = signed_in();
    let existing = custom_role_row(RoleId::now_v7(), "releaser", Permission::ViewProject.bit());

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![existing]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/roles")
        .add_header("authorization", auth)
        .json(&json!({
            "name": "releaser",
            "permissions": ["viewProject"],
        }))
        .await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn patch_builtin_role_is_forbidden() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![admin_role_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!(
            "/api/v1/projects/test-project/roles/{}",
            BASE_ROLE_ADMIN_ID
        ))
        .add_header("authorization", auth)
        .json(&json!({"permissions": ["viewProject"]}))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn patch_custom_role_updates_mask() {
    let (db, auth) = signed_in();
    let custom_id = RoleId::now_v7();
    let custom = custom_role_row(custom_id, "releaser", Permission::ViewProject.bit());
    let updated = custom_role_row(
        custom_id,
        "releaser",
        Permission::ViewProject.bit() | Permission::TriggerEvaluation.bit(),
    );

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![custom]])
        .append_query_results([vec![updated]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(&format!(
            "/api/v1/projects/test-project/roles/{}",
            custom_id
        ))
        .add_header("authorization", auth)
        .json(&json!({
            "permissions": ["viewProject", "triggerEvaluation"],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let perms = body["message"]["permissions"].as_array().unwrap();
    assert!(perms.iter().any(|p| p == "triggerEvaluation"));
}

#[tokio::test]
async fn delete_builtin_role_is_forbidden() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![view_role_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!(
            "/api/v1/projects/test-project/roles/{}",
            BASE_ROLE_VIEW_ID
        ))
        .add_header("authorization", auth)
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_role_in_use_is_rejected() {
    let (db, auth) = signed_in();
    let custom_id = RoleId::now_v7();
    let custom = custom_role_row(custom_id, "releaser", Permission::ViewProject.bit());

    let in_use_membership = project_user::Model {
        id: ProjectUserId::now_v7(),
        project: project_id(),
        user: UserId::new(Uuid::now_v7()),
        role: custom_id,
    };

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![custom]])
        .append_query_results([vec![in_use_membership]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!(
            "/api/v1/projects/test-project/roles/{}",
            custom_id
        ))
        .add_header("authorization", auth)
        .await;

    res.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_role_granted_to_a_team_is_rejected() {
    let (db, auth) = signed_in();
    let custom_id = RoleId::now_v7();
    let custom = custom_role_row(custom_id, "releaser", Permission::ViewProject.bit());

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![custom]])
        .append_query_results::<project_user::Model, _, _>([Vec::<project_user::Model>::new()])
        .append_query_results([vec![gradient_entity::team_project::Model {
            id: TeamProjectId::now_v7(),
            team: TeamId::now_v7(),
            project: project_id(),
            role: Some(custom_id),
            includes_users: true,
            includes_workers: true,
            managed: false,
            created_at: chrono::Utc::now().naive_utc(),
        }]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!(
            "/api/v1/projects/test-project/roles/{}",
            custom_id
        ))
        .add_header("authorization", auth)
        .await;

    res.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_unused_custom_role_succeeds() {
    let (db, auth) = signed_in();
    let custom_id = RoleId::now_v7();
    let custom = custom_role_row(custom_id, "releaser", Permission::ViewProject.bit());

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![custom]])
        .append_query_results::<project_user::Model, _, _>([Vec::<project_user::Model>::new()])
        .append_query_results([Vec::<gradient_entity::team_project::Model>::new()])
        .append_query_results([Vec::<gradient_entity::team_project_request::Model>::new()])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!(
            "/api/v1/projects/test-project/roles/{}",
            custom_id
        ))
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
}
