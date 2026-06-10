/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the cache member management API
//! (`/api/v1/caches/{cache}/members`).

use gradient_entity::{cache, cache_role, cache_user, ids::*, user};
use gradient_db::permissions::{cache_admin_mask, cache_view_mask};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use gradient_test_support::fixtures::{test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("c0000000-0000-0000-0000-000000000001").unwrap())
}

fn other_user_id() -> UserId {
    UserId::new(Uuid::parse_str("c0000000-0000-0000-0000-000000000002").unwrap())
}

fn cache_row(managed: bool) -> cache::Model {
    cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "pk".into(),
        private_key: "sk".into(),
        created_by: user_id(),
        created_at: test_date(),
        managed,
        ..Default::default()
    }
}

/// Build a one-row mock result that satisfies sea-orm's `count()` parser
/// (`SELECT COUNT(*) AS num_items` → `try_get::<i64>("", "num_items")`).
fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

fn admin_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_ADMIN_ID,
    }
}

fn view_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_VIEW_ID,
    }
}

fn admin_role_row() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: cache_admin_mask(),
        managed: true,
        ..Default::default()
    }
}

fn view_role_row() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_VIEW_ID,
        name: "View".into(),
        permission: cache_view_mask(),
        managed: true,
        ..Default::default()
    }
}

fn other_user_row() -> user::Model {
    user::Model {
        id: other_user_id(),
        username: "otheruser".into(),
        name: "Other User".into(),
        email: "other@example.com".into(),
        last_login_at: test_date(),
        created_at: test_date(),
        email_verified: true,
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

// ── GET /caches/{cache}/members ───────────────────────────────────────────────

#[test]
fn list_members_requires_view_cache() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // Admin caller: cache found → member lookup (admin) → role lookup → member+user join
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // the join for member listing returns the admin member+user pair
            .append_query_results([vec![(admin_member(), Some(user()))]])
            // role map lookup
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        let members = body["message"].as_array().expect("members array");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["id"], "testuser");
        assert_eq!(members[0]["name"], "Admin");
    });
}

#[test]
fn list_members_non_member_gets_not_found() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // load_cache with ViewCache: cache found → member lookup returns empty
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([Vec::<cache_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::NOT_FOUND);
    });
}

// ── POST /caches/{cache}/members ──────────────────────────────────────────────

#[test]
fn add_member_requires_manage_cache_members() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // View-role caller: cache → member (View) → role lookup → blocked by ManageCacheMembers
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![view_member()]])
            .append_query_results([vec![view_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "otheruser", "role": "View"}))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn add_member_admin_succeeds() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let new_member = cache_user::Model {
            id: CacheUserId::now_v7(),
            cache: cache_id(),
            user: other_user_id(),
            role: BASE_CACHE_ROLE_VIEW_ID,
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_cache Require(ManageCacheMembers): cache → member → role
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // find_user_by_username
            .append_query_results([vec![other_user_row()]])
            // find_cache_membership (check not already a member) → empty
            .append_query_results([Vec::<cache_user::Model>::new()])
            // role lookup by name
            .append_query_results([vec![view_role_row()]])
            // INSERT
            .append_query_results([vec![new_member]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "otheruser", "role": "View"}))
            .await;

        res.assert_status_ok();
    });
}

// ── PATCH /caches/{cache}/members ─────────────────────────────────────────────

#[test]
fn update_member_changes_role() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let target_member = cache_user::Model {
            id: CacheUserId::now_v7(),
            cache: cache_id(),
            user: other_user_id(),
            role: BASE_CACHE_ROLE_VIEW_ID,
        };
        let updated_member = cache_user::Model {
            role: BASE_CACHE_ROLE_ADMIN_ID,
            ..target_member.clone()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // find_user_by_username
            .append_query_results([vec![other_user_row()]])
            // find_cache_membership (target)
            .append_query_results([vec![target_member]])
            // role lookup by name
            .append_query_results([vec![admin_role_row()]])
            // UPDATE
            .append_query_results([vec![updated_member]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "otheruser", "role": "Admin"}))
            .await;

        res.assert_status_ok();
    });
}

// ── DELETE /caches/{cache}/members ────────────────────────────────────────────

#[test]
fn remove_member_blocks_last_admin() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // self is the only Admin → delete should be 409
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // find_user_by_username (removing self)
            .append_query_results([vec![user()]])
            // find_cache_membership
            .append_query_results([vec![admin_member()]])
            // COUNT admin members → 1 (sea-orm count parses `num_items: i64`)
            .append_query_results([vec![count_row(1)]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "testuser"}))
            .await;

        res.assert_status(axum::http::StatusCode::CONFLICT);
    });
}

#[test]
fn remove_member_view_role_succeeds() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let target_member = cache_user::Model {
            id: CacheUserId::now_v7(),
            cache: cache_id(),
            user: other_user_id(),
            role: BASE_CACHE_ROLE_VIEW_ID,
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]])
            // find_user_by_username
            .append_query_results([vec![other_user_row()]])
            // find_cache_membership
            .append_query_results([vec![target_member]])
            // role is View → no Admin-count check; straight DELETE
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "otheruser"}))
            .await;

        res.assert_status_ok();
    });
}

#[test]
fn managed_cache_blocks_member_mutations() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // cache is managed → reject_managed triggers 403
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![cache_row(true)]])
            .append_query_results([vec![admin_member()]])
            .append_query_results([vec![admin_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch("/api/v1/caches/test-cache/members")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({"user": "otheruser", "role": "View"}))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}
