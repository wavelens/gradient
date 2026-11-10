/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_role, cache_upstream, cache_user, ids::*};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::rows::{cache_admin_role, cache_member_in, cache_view_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID};
use gradient_types::permissions::{CachePermission, cache_mask_from};
use sea_orm::MockDatabase;
use serde_json::json;

fn cache_row(name: &str, public: bool) -> cache::Model {
    cache::Model {
        id: CacheId::now_v7(),
        name: name.into(),
        display_name: name.into(),
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

fn internal_upstream(cache_id: CacheId, upstream_cache: CacheId) -> cache_upstream::Model {
    cache_upstream::Model {
        id: CacheUpstreamId::now_v7(),
        cache: cache_id,
        display_name: "upstream".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        kind: CacheUpstreamKind::Internal,
        upstream_cache: Some(upstream_cache),
        active: true,
        ..Default::default()
    }
}

fn managing_admin(db: MockDatabase, cache: &cache::Model) -> MockDatabase {
    db.append_query_results([vec![cache.clone()]])
        .append_query_results([vec![cache_member_in(cache.id, BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![cache_admin_role()]])
}

#[tokio::test]
async fn adding_a_public_upstream_in_push_mode_needs_write_store() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let upstream = cache_row("upstream", true);
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream]])
        .append_query_results([Vec::<cache_user::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .put("/api/v1/caches/main/upstream-caches")
        .add_header("authorization", auth)
        .json(&json!({ "type": "internal", "cache_name": "upstream" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let message = refusal_message(&res);
    assert!(message.contains("\"write cache\""), "{message}");
    assert!(!message.contains("writeStore"), "{message}");
}

#[tokio::test]
async fn adding_an_upstream_as_viewer_in_write_only_mode_is_refused() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let upstream = cache_row("upstream", false);
    let viewer = cache_member_in(upstream.id, BASE_CACHE_ROLE_VIEW_ID);
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream]])
        .append_query_results([vec![viewer.clone()]])
        .append_query_results([vec![cache_view_role()]])
        .append_query_results([vec![viewer]])
        .append_query_results([vec![cache_view_role()]]);

    let res = make_test_server(db.into_connection())
        .put("/api/v1/caches/main/upstream-caches")
        .add_header("authorization", auth)
        .json(&json!({ "type": "internal", "cache_name": "upstream", "mode": "WriteOnly" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

fn refusal_message(res: &axum_test::TestResponse) -> String {
    res.json::<serde_json::Value>()["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn adding_a_private_upstream_with_only_view_cache_in_read_only_mode_needs_read_store() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let upstream = cache_row("upstream", false);
    let view_only = cache_role::Model {
        id: RoleId::now_v7(),
        cache: Some(upstream.id),
        name: "view only".into(),
        permission: cache_mask_from(&[CachePermission::ViewCache]),
        ..Default::default()
    };
    let member = cache_member_in(upstream.id, view_only.id);
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream]])
        .append_query_results([vec![member.clone()]])
        .append_query_results([vec![view_only.clone()]])
        .append_query_results([vec![member]])
        .append_query_results([vec![view_only]]);

    let res = make_test_server(db.into_connection())
        .put("/api/v1/caches/main/upstream-caches")
        .add_header("authorization", auth)
        .json(&json!({ "type": "internal", "cache_name": "upstream", "mode": "ReadOnly" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let message = refusal_message(&res);
    assert!(message.contains("\"read cache\""), "{message}");
}

#[tokio::test]
async fn switching_an_upstream_to_read_write_needs_write_store() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let target = cache_row("upstream", true);
    let upstream = internal_upstream(main.id, target.id);
    let upstream_id = upstream.id;
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream]])
        .append_query_results([vec![target]])
        .append_query_results([Vec::<cache_user::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .patch(&format!(
            "/api/v1/caches/main/upstream-caches/{upstream_id}"
        ))
        .add_header("authorization", auth)
        .json(&json!({ "mode": "ReadWrite" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let message = refusal_message(&res);
    assert!(message.contains("\"write cache\""), "{message}");
}

#[tokio::test]
async fn switching_an_upstream_of_a_private_cache_to_read_only_needs_read_store() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let target = cache_row("upstream", false);
    let upstream = internal_upstream(main.id, target.id);
    let upstream_id = upstream.id;
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream]])
        .append_query_results([vec![target]])
        .append_query_results([Vec::<cache_user::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .patch(&format!(
            "/api/v1/caches/main/upstream-caches/{upstream_id}"
        ))
        .add_header("authorization", auth)
        .json(&json!({ "mode": "ReadOnly" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let message = refusal_message(&res);
    assert!(message.contains("\"read cache\""), "{message}");
}

#[tokio::test]
async fn switching_an_upstream_of_a_public_cache_to_read_only_needs_no_store_permission() {
    let (db, auth) = signed_in();
    let main = cache_row("main", false);
    let target = cache_row("upstream", true);
    let upstream = internal_upstream(main.id, target.id);
    let upstream_id = upstream.id;
    let db = managing_admin(db, &main)
        .append_query_results([vec![upstream.clone()]])
        .append_query_results([vec![target]])
        .append_query_results([vec![upstream]]);

    let res = make_test_server(db.into_connection())
        .patch(&format!(
            "/api/v1/caches/main/upstream-caches/{upstream_id}"
        ))
        .add_header("authorization", auth)
        .json(&json!({ "mode": "ReadOnly" }))
        .await;

    res.assert_status_ok();
}
