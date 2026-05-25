/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the cache-scoped role management API
//! (`/api/v1/caches/{cache}/roles`).

use entity::{cache, cache_role, cache_user, ids::*};
use gradient_core::permissions::{cache_admin_mask, cache_view_mask};
use gradient_core::types::SessionId;
use gradient_core::types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};
use test_support::fixtures::{test_date, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("d0000000-0000-0000-0000-000000000001").unwrap())
}

fn cache_row() -> cache::Model {
    cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "pk".into(),
        private_key: "sk".into(),
        public: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
    }
}

fn admin_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_ADMIN_ID,
    }
}

fn admin_role_row() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        cache: None,
        permission: cache_admin_mask(),
        managed: true,
    }
}

fn view_role_row() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_VIEW_ID,
        name: "View".into(),
        cache: None,
        permission: cache_view_mask(),
        managed: true,
    }
}

fn custom_role_row(id: RoleId, name: &str, permission: i64) -> cache_role::Model {
    cache_role::Model {
        id,
        name: name.into(),
        cache: Some(cache_id()),
        permission,
        managed: false,
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

// ── GET /caches/{cache}/roles ─────────────────────────────────────────────────

#[test]
fn list_returns_builtins_and_available_permissions() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_cache Member: cache → cache_user membership
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            // role listing (built-ins + custom)
            .append_query_results([vec![
                admin_role_row(),
                view_role_row(),
                custom_role_row(custom_id, "pusher", 0),
            ]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/caches/test-cache/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        let roles = body["message"]["roles"].as_array().expect("roles");
        assert!(roles.len() >= 2);
        assert!(
            roles
                .iter()
                .any(|r| r["name"] == "Admin" && r["builtin"] == true)
        );
        assert!(
            roles
                .iter()
                .any(|r| r["name"] == "pusher" && r["builtin"] == false)
        );
        let perms = body["message"]["available_permissions"]
            .as_array()
            .expect("available_permissions");
        assert!(perms.iter().any(|p| p["id"] == "manageCacheMembers"));
    });
}

// ── POST /caches/{cache}/roles ────────────────────────────────────────────────

#[test]
fn create_role_rejects_duplicate_name() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let existing = custom_role_row(RoleId::now_v7(), "pusher", 0);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // name clash pre-check returns existing row
            .append_query_results([vec![existing]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/caches/test-cache/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"name": "pusher", "permissions": []}))
            .await;

        res.assert_status(axum::http::StatusCode::CONFLICT);
    });
}

#[test]
fn create_role_rejects_unknown_permission() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/caches/test-cache/roles")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"name": "pusher", "permissions": ["notARealPermission"]}))
            .await;

        res.assert_status(axum::http::StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("notARealPermission")
        );
    });
}

// ── PATCH /caches/{cache}/roles/{role_id} ─────────────────────────────────────

#[test]
fn patch_role_rejects_builtin() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // load_cache_role returns the Admin built-in
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!(
                "/api/v1/caches/test-cache/roles/{}",
                BASE_CACHE_ROLE_ADMIN_ID
            ))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"permissions": []}))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn patch_role_rejects_managed() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();
        let managed_custom = cache_role::Model {
            managed: true,
            ..custom_role_row(custom_id, "pusher", 0)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([vec![managed_custom]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("/api/v1/caches/test-cache/roles/{}", custom_id))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"permissions": []}))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

// ── DELETE /caches/{cache}/roles/{role_id} ────────────────────────────────────

#[test]
fn delete_role_rejects_role_in_use() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let custom_id = RoleId::now_v7();
        let custom = custom_role_row(custom_id, "pusher", 0);

        let in_use = cache_user::Model {
            id: CacheUserId::now_v7(),
            cache: cache_id(),
            user: user_id(),
            role: custom_id,
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // load_cache_role
            .append_query_results([vec![custom]])
            // in-use check returns a member
            .append_query_results([vec![in_use]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("/api/v1/caches/test-cache/roles/{}", custom_id))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::BAD_REQUEST);
    });
}
