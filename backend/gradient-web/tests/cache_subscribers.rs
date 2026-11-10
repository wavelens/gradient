/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{api, cache, ids::*, project, project_cache, project_user};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::{project_with_id, user, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::make_test_server;
use gradient_types::consts::BASE_ROLE_VIEW_ID;
use gradient_types::permissions::{Permission, mask_from};
use gradient_web::authorization::hash_api_key;
use serde_json::Value;

fn public_cache() -> cache::Model {
    cache::Model {
        id: CacheId::now_v7(),
        name: "main".into(),
        display_name: "Main".into(),
        active: true,
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn subscription(cache: CacheId, project: ProjectId) -> project_cache::Model {
    project_cache::Model {
        id: ProjectCacheId::now_v7(),
        project,
        cache,
        mode: CacheSubscriptionMode::ReadWrite,
    }
}

#[tokio::test]
async fn anonymous_sees_public_subscribers_and_a_count_of_the_rest() {
    let cache = public_cache();
    let open = project::Model {
        public: true,
        display_name: "Open Project".into(),
        ..project_with_id(ProjectId::now_v7(), "open")
    };
    let private = project_with_id(ProjectId::now_v7(), "private");
    let db = mock_db()
        .append_query_results([vec![cache.clone()]])
        .append_query_results([vec![
            subscription(cache.id, open.id),
            subscription(cache.id, private.id),
        ]])
        .append_query_results([vec![open, private]]);

    let server = make_test_server(db.into_connection());
    let res = server.get("/api/v1/caches/main/subscribers").await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"]["projects"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(body["message"]["projects"][0]["project"], "open");
    assert_eq!(
        body["message"]["projects"][0]["project_display_name"],
        "Open Project"
    );
    assert_eq!(
        body["message"]["hidden"], 1,
        "a private subscriber is counted, never named"
    );
}

fn view_member_of(project: ProjectId) -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::now_v7(),
        project,
        user: user_id(),
        role: BASE_ROLE_VIEW_ID,
    }
}

#[tokio::test]
async fn a_project_pinned_key_does_not_name_the_other_private_subscribers() {
    let raw = "p".repeat(64);
    let cache = public_cache();
    let pinned = project_with_id(ProjectId::now_v7(), "pinned");
    let other = project_with_id(ProjectId::now_v7(), "other");
    let now = chrono::Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "pinned".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(Permission::ALL),
        project: Some(pinned.id),
        ..Default::default()
    };
    let db = mock_db()
        .append_query_results([vec![key.clone()]])
        .append_exec_results([exec_ok()])
        .append_query_results([vec![key]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![cache.clone()]])
        .append_query_results([vec![
            subscription(cache.id, pinned.id),
            subscription(cache.id, other.id),
        ]])
        .append_query_results([vec![pinned.clone(), other.clone()]])
        .append_query_results([vec![view_member_of(pinned.id), view_member_of(other.id)]])
        .append_query_results([vec![view_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/caches/main/subscribers")
        .add_header("authorization", format!("Bearer GRAD{raw}"))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let named: Vec<&str> = body["message"]["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["project"].as_str())
        .collect();
    assert_eq!(named, ["pinned"]);
    assert_eq!(body["message"]["hidden"], 1);
}
