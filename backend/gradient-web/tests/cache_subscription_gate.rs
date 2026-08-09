/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the bilateral permission gate on project→cache subscription.
//!
//! Each POST /projects/{project}/subscribe/{cache} requires:
//!   - ManageSubscriptions on the project (project-side)
//!   - ManageCacheSubscriptions on the cache (cache-side)
//!
//! Auth query sequence (authorize middleware):
//!   1. SELECT session
//!   2. UPDATE session (last_used_at)
//!   3. SELECT user
//!
//! load_project with Require(ManageSubscriptions):
//!   4. SELECT project (by name)
//!   5. SELECT project_user (membership)
//!   6. SELECT role (bitmask)
//!
//! load_cache with Require(ManageCacheSubscriptions):
//!   7. SELECT cache (by name)
//!   8. SELECT cache_user (membership)
//!   9. SELECT cache_role (bitmask)  - only when member exists

use gradient_db::permissions::{admin_mask, cache_admin_mask, cache_view_mask, mask_from};
use gradient_entity::{cache, cache_role, cache_user, ids::*, project_cache, project_user, role};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID,
};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use uuid::Uuid;

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("c1000000-0000-0000-0000-000000000001").unwrap())
}

fn project_user_id() -> ProjectUserId {
    ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap())
}

fn cache_row(public: bool) -> cache::Model {
    cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "pk".into(),
        private_key: "sk".into(),
        public,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn admin_project_membership() -> project_user::Model {
    project_user::Model {
        id: project_user_id(),
        project: project_id(),
        user: user_id(),
        role: BASE_ROLE_ADMIN_ID,
    }
}

fn view_only_project_role() -> role::Model {
    role::Model {
        id: RoleId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000f1").unwrap()),
        name: "ViewOnly".into(),
        project: Some(project_id()),
        permission: mask_from(&[gradient_db::permissions::Permission::ViewProject]),
        ..Default::default()
    }
}

fn view_only_project_membership() -> project_user::Model {
    project_user::Model {
        id: project_user_id(),
        project: project_id(),
        user: user_id(),
        role: view_only_project_role().id,
    }
}

fn admin_project_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: admin_mask(),
        ..Default::default()
    }
}

fn admin_cache_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_ADMIN_ID,
    }
}

fn view_cache_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_VIEW_ID,
    }
}

fn admin_cache_role() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: cache_admin_mask(),
        managed: true,
        ..Default::default()
    }
}

fn view_cache_role() -> cache_role::Model {
    cache_role::Model {
        id: BASE_CACHE_ROLE_VIEW_ID,
        name: "View".into(),
        permission: cache_view_mask(),
        managed: true,
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn subscribe_requires_project_manage_subscriptions() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // User has ViewProject-only on the project (no ManageSubscriptions) → 403.
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![view_only_project_membership()]])
            .append_query_results([vec![view_only_project_role()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn subscribe_requires_cache_manage_subscriptions() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // User has Admin on project (ManageSubscriptions passes) but only View on
        // the cache (no ManageCacheSubscriptions) → 403.
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![view_cache_member()]])
            .append_query_results([vec![view_cache_role()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn subscribe_succeeds_when_both_granted() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let inserted_link = project_cache::Model {
            id: ProjectCacheId::now_v7(),
            project: project_id(),
            cache: cache_id(),
            mode: project_cache::CacheSubscriptionMode::ReadWrite,
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_project
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            // load_cache
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_cache_member()]])
            .append_query_results([vec![admin_cache_role()]])
            // already-subscribed check → empty
            .append_query_results([Vec::<project_cache::Model>::new()])
            // insert project_cache row
            .append_query_results([vec![inserted_link]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // unpark_no_cache_for_project: tasks query → empty (short-circuits)
            .append_query_results([Vec::<gradient_entity::task::Model>::new()])
            // enqueue_backfill_signatures: derivations query → empty (short-circuits)
            .append_query_results([Vec::<gradient_entity::derivation::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["error"], false);
    });
}

#[test]
fn subscribe_public_cache_still_requires_cache_permission() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        // cache.public = true but user has no cache_user row →
        // Require(ManageCacheSubscriptions) returns 404 (not_found) because
        // the member lookup finds no row and the label is "Cache".
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            .append_query_results([vec![cache_row(true)]])
            .append_query_results([Vec::<cache_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        // Require with no member row → not_found (no longer bypassed for public caches)
        res.assert_status(axum::http::StatusCode::NOT_FOUND);
    });
}
