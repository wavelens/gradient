/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_db::permissions::{admin_mask, cache_admin_mask, cache_view_mask, mask_from};
use gradient_entity::{
    cache, cache_role, cache_subscription_request, cache_user, ids::*, project_cache, project_user,
    role,
};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID,
};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use uuid::Uuid;

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

#[test]
fn subscribe_requires_project_manage_subscriptions() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

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
fn subscribe_without_cache_permission_records_a_request() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let pending = cache_subscription_request::Model {
            id: CacheSubscriptionRequestId::now_v7(),
            project: project_id(),
            cache: cache_id(),
            mode: project_cache::CacheSubscriptionMode::ReadWrite,
            requested_by: user_id(),
            created_at: test_date(),
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![view_cache_member()]])
            .append_query_results([Vec::<project_cache::Model>::new()])
            .append_query_results([Vec::<cache_subscription_request::Model>::new()])
            .append_query_results([vec![view_cache_member()]])
            .append_query_results([vec![view_cache_role()]])
            .append_query_results([vec![pending]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<cache_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(
            body["message"], "Subscription requested",
            "no cache-side permission means a request, not a subscription"
        );
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
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            .append_query_results([vec![cache_row(false)]])
            .append_query_results([vec![admin_cache_member()]])
            .append_query_results([Vec::<project_cache::Model>::new()])
            .append_query_results([Vec::<cache_subscription_request::Model>::new()])
            .append_query_results([vec![admin_cache_member()]])
            .append_query_results([vec![admin_cache_role()]])
            .append_query_results([vec![inserted_link]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<gradient_entity::task::Model>::new()])
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
fn subscribe_to_a_public_cache_records_a_request_for_a_non_member() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let pending = cache_subscription_request::Model {
            id: CacheSubscriptionRequestId::now_v7(),
            project: project_id(),
            cache: cache_id(),
            mode: project_cache::CacheSubscriptionMode::ReadWrite,
            requested_by: user_id(),
            created_at: test_date(),
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([vec![admin_project_membership()]])
            .append_query_results([vec![admin_project_role()]])
            .append_query_results([vec![cache_row(true)]])
            .append_query_results([Vec::<project_cache::Model>::new()])
            .append_query_results([Vec::<cache_subscription_request::Model>::new()])
            .append_query_results([Vec::<cache_user::Model>::new()])
            .append_query_results([vec![pending]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<cache_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/projects/test-project/subscribe/test-cache")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["message"], "Subscription requested");
    });
}
