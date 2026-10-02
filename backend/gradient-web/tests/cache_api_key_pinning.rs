/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_db::permissions::{cache_admin_mask, cache_view_mask};
use gradient_entity::{api, cache, cache_role, cache_user, ids::*};
use gradient_test_support::fixtures::{test_date, user, user_id};
use gradient_test_support::web::make_test_server;
use gradient_types::consts::BASE_CACHE_ROLE_VIEW_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use sha2::{Digest, Sha256};

fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn cache_id() -> CacheId {
    CacheId::new(uuid::uuid!("c1000000-0000-0000-0000-000000000001"))
}

fn other_cache_id() -> CacheId {
    CacheId::new(uuid::uuid!("c2000000-0000-0000-0000-000000000002"))
}

fn cache_row() -> cache::Model {
    cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "pk".into(),
        private_key: "sk".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn private_cache_row() -> cache::Model {
    cache::Model {
        public: false,
        ..cache_row()
    }
}

fn pinned_api_key(raw: &str, pin: CacheId, permission: i64) -> api::Model {
    let now = chrono::Utc::now().naive_utc();
    api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "pinned-key".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission,
        cache: Some(pin),
        ..Default::default()
    }
}

fn api_key_db(db: MockDatabase, key: &api::Model) -> MockDatabase {
    db.append_query_results([vec![key.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![user()]])
}

fn view_cache_member() -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id(),
        user: user_id(),
        role: BASE_CACHE_ROLE_VIEW_ID,
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

#[tokio::test]
async fn cache_pinned_key_works_on_pinned_cache() {
    let raw = "a".repeat(64);
    let key = pinned_api_key(&raw, cache_id(), cache_admin_mask());

    let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/caches/test-cache")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;

    res.assert_status_ok();
    let body: serde_json::Value = res.json();
    assert_eq!(body["error"], false);
}

#[tokio::test]
async fn cache_pinned_key_rejected_on_other_cache() {
    let raw = "b".repeat(64);
    let key = pinned_api_key(&raw, other_cache_id(), cache_admin_mask());

    let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
        .append_query_results([vec![cache_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/caches/test-cache")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cache_pinned_key_rejected_on_project_endpoint() {
    let raw = "c".repeat(64);
    let key = pinned_api_key(&raw, cache_id(), cache_admin_mask());

    let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
        .append_query_results([vec![gradient_test_support::fixtures::project()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/projects/test-project")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_key_rejects_both_project_and_cache_pin() {
    // A session JWT is used because API keys cannot create API keys.
    let session_id = gradient_types::SessionId::now_v7();
    let token = gradient_test_support::web::make_token(session_id);
    let session = gradient_test_support::web::live_session(session_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "bad-key",
            "permissions": ["viewCache"],
            "project": "test-project",
            "cache": "test-cache",
        }))
        .await;

    res.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn create_cache_pinned_key_cannot_exceed_member_mask() {
    let session_id = gradient_types::SessionId::now_v7();
    let token = gradient_test_support::web::make_token(session_id);
    let session = gradient_test_support::web::live_session(session_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([Vec::<api::Model>::new()])
        .append_query_results([vec![private_cache_row()]])
        .append_query_results([vec![view_cache_member()]])
        .append_query_results([vec![view_cache_member()]])
        .append_query_results([vec![view_cache_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "my-cache-key",
            "permissions": ["writeStore"],
            "cache": "test-cache",
        }))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}
