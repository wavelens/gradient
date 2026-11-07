/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, ids::*, project, project_cache};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::project_with_id;
use gradient_fixtures::test_date;
use gradient_test_support::web::make_test_server;
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
