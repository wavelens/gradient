/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the org-scoped role-management API
//! (`/api/v1/orgs/{org}/roles`). Covers built-in role discovery, custom role
//! creation, immutability of built-ins, name uniqueness, and the
//! reassign-before-delete invariant.
//!
//! `MockDatabase` replays canned query results in FIFO order, so each test
//! script is "auth (3 selects) → load_org → load_membership → load_role →
//! …handler-specific…". Where the handler runs `INSERT … RETURNING …` we feed
//! the inserted row in via `append_query_results` *and* match up an
//! `append_exec_results` with `rows_affected: 1`, otherwise SeaORM treats the
//! insert as a no-op and short-circuits.

use gradient_entity::{ids::*, organization_user, role};
use gradient_db::permissions::{Permission, admin_mask, view_mask, write_mask};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use gradient_test_support::fixtures::{org, org_id, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn admin_membership() -> organization_user::Model {
    organization_user::Model {
        id: OrganizationUserId::now_v7(),
        organization: org_id(),
        user: user_id(),
        role: BASE_ROLE_ADMIN_ID,
    }
}

fn view_membership() -> organization_user::Model {
    organization_user::Model {
        id: OrganizationUserId::now_v7(),
        organization: org_id(),
        user: user_id(),
        role: BASE_ROLE_VIEW_ID,
    }
}

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
        organization: Some(org_id()),
        permission,
        ..Default::default()
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

// ── GET /orgs/{org}/roles ────────────────────────────────────────────────────

#[test]
fn list_roles_returns_builtins_plus_custom() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();

        // GET requires `Member` access, which is membership-existence only -
        // no role-row lookup is performed.
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![view_membership()]])
            .append_query_results([vec![
                admin_role_row(),
                write_role_row(),
                view_role_row(),
                custom_role_row(custom_id, "releaser", Permission::TriggerEvaluation.bit()),
            ]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/orgs/test-org/roles")
            .add_header("authorization", format!("Bearer {}", token))
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
    });
}

// ── POST /orgs/{org}/roles ───────────────────────────────────────────────────

#[test]
fn create_role_persists_permission_bitmask() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let inserted = custom_role_row(
            RoleId::now_v7(),
            "releaser",
            Permission::TriggerEvaluation.bit() | Permission::ViewOrg.bit(),
        );

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_org -> Require(ManageRoles)
            .append_query_results([vec![org()]])
            // load_membership_with_permissions: membership + role
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            // name uniqueness pre-check returns no row
            .append_query_results::<role::Model, _, _>([Vec::<role::Model>::new()])
            // INSERT ... RETURNING the new row
            .append_query_results([vec![inserted.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/orgs/test-org/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "releaser",
                "permissions": ["triggerEvaluation", "viewOrg"],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["name"], "releaser");
        assert_eq!(body["message"]["builtin"], false);
        let perms = body["message"]["permissions"].as_array().unwrap();
        assert!(perms.iter().any(|p| p == "triggerEvaluation"));
        assert!(perms.iter().any(|p| p == "viewOrg"));
    });
}

#[test]
fn create_role_rejects_unknown_permission() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/orgs/test-org/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "releaser",
                "permissions": ["banana"],
            }))
            .await;

        res.assert_status(axum::http::StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(body["message"].as_str().unwrap().contains("banana"));
    });
}

#[test]
fn create_role_rejects_view_role_caller() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![view_membership()]])
            .append_query_results([vec![view_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/orgs/test-org/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "releaser",
                "permissions": ["triggerEvaluation"],
            }))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn create_role_rejects_duplicate_name() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let existing = custom_role_row(RoleId::now_v7(), "releaser", Permission::ViewOrg.bit());

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            // name uniqueness pre-check finds an existing custom role.
            .append_query_results([vec![existing]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/orgs/test-org/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "releaser",
                "permissions": ["viewOrg"],
            }))
            .await;

        res.assert_status(axum::http::StatusCode::CONFLICT);
    });
}

// ── PATCH /orgs/{org}/roles/{id} ─────────────────────────────────────────────

#[test]
fn patch_builtin_role_is_forbidden() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            // load_org_role returns the built-in
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!(
                "/api/v1/orgs/test-org/roles/{}",
                BASE_ROLE_ADMIN_ID
            ))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"permissions": ["viewOrg"]}))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn patch_custom_role_updates_mask() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();
        let custom = custom_role_row(custom_id, "releaser", Permission::ViewOrg.bit());
        let updated = custom_role_row(
            custom_id,
            "releaser",
            Permission::ViewOrg.bit() | Permission::TriggerEvaluation.bit(),
        );

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![custom]])
            .append_query_results([vec![updated]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("/api/v1/orgs/test-org/roles/{}", custom_id))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "permissions": ["viewOrg", "triggerEvaluation"],
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let perms = body["message"]["permissions"].as_array().unwrap();
        assert!(perms.iter().any(|p| p == "triggerEvaluation"));
    });
}

// ── DELETE /orgs/{org}/roles/{id} ────────────────────────────────────────────

#[test]
fn delete_builtin_role_is_forbidden() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![view_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!(
                "/api/v1/orgs/test-org/roles/{}",
                BASE_ROLE_VIEW_ID
            ))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn delete_role_in_use_is_rejected() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();
        let custom = custom_role_row(custom_id, "releaser", Permission::ViewOrg.bit());

        let in_use_membership = organization_user::Model {
            id: OrganizationUserId::now_v7(),
            organization: org_id(),
            user: UserId::new(Uuid::now_v7()),
            role: custom_id,
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![custom]])
            .append_query_results([vec![in_use_membership]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("/api/v1/orgs/test-org/roles/{}", custom_id))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::BAD_REQUEST);
    });
}

#[test]
fn delete_unused_custom_role_succeeds() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();
        let custom = custom_role_row(custom_id, "releaser", Permission::ViewOrg.bit());

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![custom]])
            .append_query_results::<organization_user::Model, _, _>([
                Vec::<organization_user::Model>::new(),
            ])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("/api/v1/orgs/test-org/roles/{}", custom_id))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
    });
}
