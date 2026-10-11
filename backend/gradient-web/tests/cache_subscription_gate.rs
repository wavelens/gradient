/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::{cache, cache_subscription_request, cache_user, ids::*, project_cache, role};
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::rows::{
    admin_role, cache_admin_role, cache_member_in, cache_view_role, project, project_member,
};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID,
};
use gradient_types::permissions::mask_from;
use sea_orm::MockExecResult;
use uuid::Uuid;

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("c1000000-0000-0000-0000-000000000001").unwrap())
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

fn view_only_project_role() -> role::Model {
    role::Model {
        id: RoleId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000f1").unwrap()),
        name: "ViewOnly".into(),
        project: Some(project_id()),
        permission: mask_from(&[gradient_types::permissions::Permission::ViewProject]),
        ..Default::default()
    }
}

#[tokio::test]
async fn subscribe_requires_project_manage_subscriptions() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(view_only_project_role().id)]])
        .append_query_results([vec![view_only_project_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", auth)
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn subscribe_without_cache_permission_records_a_request() {
    let (db, auth) = signed_in();

    let pending = cache_subscription_request::Model {
        id: CacheSubscriptionRequestId::now_v7(),
        project: project_id(),
        cache: cache_id(),
        mode: project_cache::CacheSubscriptionMode::ReadWrite,
        requested_by: user_id(),
        created_at: test_date(),
    };

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![cache_row(false)]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([Vec::<project_cache::Model>::new()])
        .append_query_results([Vec::<cache_subscription_request::Model>::new()])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]])
        .append_query_results([vec![pending]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<cache_user::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: serde_json::Value = res.json();
    assert_eq!(
        body["message"], "Subscription requested",
        "no cache-side permission means a request, not a subscription"
    );
}

#[tokio::test]
async fn subscribe_succeeds_when_both_granted() {
    let (db, auth) = signed_in();

    let inserted_link = project_cache::Model {
        id: ProjectCacheId::now_v7(),
        project: project_id(),
        cache: cache_id(),
        mode: project_cache::CacheSubscriptionMode::ReadWrite,
    };

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![cache_row(false)]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([Vec::<project_cache::Model>::new()])
        .append_query_results([Vec::<cache_subscription_request::Model>::new()])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![cache_admin_role()]])
        .append_query_results([vec![inserted_link]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
        .append_query_results([Vec::<gradient_entity::team_project::Model>::new()])
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .append_query_results([Vec::<gradient_entity::derivation::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: serde_json::Value = res.json();
    assert_eq!(body["error"], false);
}

#[tokio::test]
async fn subscribe_to_a_public_cache_records_a_request_for_a_non_member() {
    let (db, auth) = signed_in();

    let pending = cache_subscription_request::Model {
        id: CacheSubscriptionRequestId::now_v7(),
        project: project_id(),
        cache: cache_id(),
        mode: project_cache::CacheSubscriptionMode::ReadWrite,
        requested_by: user_id(),
        created_at: test_date(),
    };

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
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
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: serde_json::Value = res.json();
    assert_eq!(body["message"], "Subscription requested");
}
