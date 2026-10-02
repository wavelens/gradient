/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_upstream, ids::*};
use gradient_test_support::fixtures::{test_date, user_id};
use gradient_test_support::web::make_test_server;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

const NIXOS_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

fn cache_row(public: bool) -> cache::Model {
    cache::Model {
        id: CacheId::now_v7(),
        name: "main".into(),
        display_name: "Main".into(),
        active: true,
        priority: 50,
        public_key: "main-pub-key".into(),
        public,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn upstream_row(cache_id: CacheId) -> cache_upstream::Model {
    cache_upstream::Model {
        id: CacheUpstreamId::now_v7(),
        cache: cache_id,
        display_name: "cache.nixos.org".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        kind: CacheUpstreamKind::Http,
        url: Some("https://cache.nixos.org".into()),
        public_key: Some(NIXOS_KEY.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn anonymous_lists_public_cache_upstream_keys() {
    let cache = cache_row(true);
    let upstream = upstream_row(cache.id);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache]])
        .append_query_results([vec![upstream]]);

    let server = make_test_server(db.into_connection());
    let res = server.get("/api/v1/caches/main/upstream-caches").await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"][0]["public_key"], NIXOS_KEY);
}

#[tokio::test]
async fn anonymous_cannot_list_private_upstream_caches() {
    let cache = cache_row(false);
    let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![cache]]);

    let server = make_test_server(db.into_connection());
    let res = server.get("/api/v1/caches/main/upstream-caches").await;

    res.assert_status(axum::http::StatusCode::NOT_FOUND);
}
