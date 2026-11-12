/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::{api, cache, ids::*};
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db};
use gradient_fixtures::rows::{
    cache_member_in, cache_view_role, project_cache_link, project_member, user,
};
use gradient_fixtures::test_date;
use gradient_test_support::web::make_test_server;
use gradient_types::consts::{BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_VIEW_ID};
use gradient_types::permissions::{CachePermission, cache_admin_mask, cache_mask_from};
use sea_orm::{MockDatabase, MockExecResult, Value as SqlValue};
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
        cache_permission: permission,
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

#[tokio::test]
async fn cache_pinned_key_works_on_pinned_cache() {
    let raw = "a".repeat(64);
    let key = pinned_api_key(&raw, cache_id(), cache_admin_mask());

    let db = api_key_db(mock_db(), &key)
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

    let db = api_key_db(mock_db(), &key).append_query_results([vec![cache_row()]]);

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

    let db = api_key_db(mock_db(), &key)
        .append_query_results([vec![gradient_fixtures::rows::project()]]);

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
    let session = gradient_fixtures::rows::live_session(session_id);

    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "bad-key",
            "cache_permissions": ["viewCache"],
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
    let session = gradient_fixtures::rows::live_session(session_id);

    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([Vec::<api::Model>::new()])
        .append_query_results([vec![private_cache_row()]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "my-cache-key",
            "cache_permissions": ["writeStore"],
            "cache": "test-cache",
        }))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<serde_json::Value>()["missing_permissions"],
        serde_json::json!(["writeStore"])
    );
}

#[tokio::test]
async fn subscribed_project_member_cannot_create_a_key_pinned_to_the_cache() {
    let session_id = gradient_types::SessionId::now_v7();
    let token = gradient_test_support::web::make_token(session_id);
    let session = gradient_fixtures::rows::live_session(session_id);

    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([Vec::<api::Model>::new()])
        .append_query_results([vec![private_cache_row()]])
        .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
        .append_query_results([vec![project_cache_link(
            ProjectCacheId::now_v7(),
            project_id(),
            cache_id(),
            gradient_entity::project_cache::CacheSubscriptionMode::ReadOnly,
        )]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "subscriber-key",
            "cache_permissions": ["readStore"],
            "cache": "test-cache",
        }))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_cache_pinned_key_rejects_project_permissions() {
    let session_id = gradient_types::SessionId::now_v7();
    let token = gradient_test_support::web::make_token(session_id);
    let session = gradient_fixtures::rows::live_session(session_id);

    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([Vec::<api::Model>::new()])
        .append_query_results([vec![private_cache_row()]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "my-cache-key",
            "permissions": ["viewProject"],
            "cache_permissions": ["viewCache"],
            "cache": "test-cache",
        }))
        .await;

    res.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_permissions_of_cache_pinned_api_key_writes_cache_permission_column_and_keeps_project_permission_zero()
 {
    let session_id = gradient_types::SessionId::now_v7();
    let token = gradient_test_support::web::make_token(session_id);
    let session = gradient_fixtures::rows::live_session(session_id);
    let key = pinned_api_key(
        &"p".repeat(64),
        cache_id(),
        cache_mask_from(&[CachePermission::ViewCache]),
    );
    let edited = cache_mask_from(&[CachePermission::ViewCache, CachePermission::ReadStore]);
    let conn = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![cache_member_in(cache_id(), BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]])
        .append_query_results([vec![api::Model {
            cache_permission: edited,
            ..key.clone()
        }]])
        .append_exec_results([exec_ok()])
        .append_query_results([vec![private_cache_row()]])
        .into_connection();

    let res = make_test_server(conn.clone())
        .patch(&format!("/api/v1/user/keys/{}", key.id))
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "cache_permissions": ["viewCache", "readStore"] }))
        .await;

    res.assert_status_ok();
    let update = all_statements(conn.into_transaction_log())
        .into_iter()
        .find(|statement| statement.sql.starts_with("UPDATE \"api\""))
        .expect("the key row is updated");
    let column_value = |column: &str| {
        let marker = format!("\"{column}\" = $");
        let index: usize = update.sql
            [update.sql.find(&marker).expect("column is set") + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap();
        update.values.as_ref().unwrap().0[index - 1].clone()
    };
    assert_eq!(
        column_value("cache_permission"),
        SqlValue::BigInt(Some(edited))
    );
    assert_eq!(column_value("permission"), SqlValue::BigInt(Some(0)));
}
