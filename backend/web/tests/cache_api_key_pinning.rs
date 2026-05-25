/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for cache-pinned API keys (Task 17).
//!
//! Auth query sequence for GRAD tokens (see auth_hardening.rs for reference):
//!   1. SELECT api  (key lookup by hash)
//!   2. EXEC        (UPDATE last_used_at via save)
//!   3. SELECT api  (re-select after save)
//!   4. SELECT user

use entity::{api, cache, cache_role, cache_user, ids::*};
use gradient_core::permissions::{cache_admin_mask, cache_view_mask};
use gradient_core::types::consts::BASE_CACHE_ROLE_VIEW_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use sha2::{Digest, Sha256};
use test_support::fixtures::{test_date, user, user_id};
use test_support::web::make_test_server;

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
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "pk".into(),
        private_key: "sk".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
    }
}

fn private_cache_row() -> cache::Model {
    cache::Model { public: false, ..cache_row() }
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
        managed: false,
        expires_at: None,
        revoked_at: None,
        permission,
        organization: None,
        cache: Some(pin),
    }
}

fn api_key_db(db: MockDatabase, key: &api::Model) -> MockDatabase {
    db.append_query_results([vec![key.clone()]])
        .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
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
        cache: None,
        permission: cache_view_mask(),
        managed: true,
    }
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn cache_pinned_key_works_on_pinned_cache() {
    run(async {
        let raw = "a".repeat(64);
        let key = pinned_api_key(&raw, cache_id(), cache_admin_mask());

        // Public cache: load_cache(Readable) finds the cache, pin matches, no member lookup.
        let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
            .append_query_results([vec![cache_row()]])
            // get_cache handler: can_edit member lookup (no row = not editable; not an error)
            .append_query_results([Vec::<entity::cache_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/caches/test-cache")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["error"], false);
    });
}

#[test]
fn cache_pinned_key_rejected_on_other_cache() {
    run(async {
        let raw = "b".repeat(64);
        // Key is pinned to other_cache_id, but request targets cache_id.
        let key = pinned_api_key(&raw, other_cache_id(), cache_admin_mask());

        let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
            .append_query_results([vec![cache_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/caches/test-cache")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn cache_pinned_key_rejected_on_org_endpoint() {
    run(async {
        let raw = "c".repeat(64);
        let key = pinned_api_key(&raw, cache_id(), cache_admin_mask());

        let db = api_key_db(MockDatabase::new(DatabaseBackend::Postgres), &key)
            .append_query_results([vec![test_support::fixtures::org()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/orgs/test-org")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn create_key_rejects_both_org_and_cache_pin() {
    // This uses a session JWT because API keys cannot create API keys.
    run(async {
        let session_id = gradient_core::types::SessionId::now_v7();
        let token = test_support::web::make_token(session_id);
        let session = test_support::web::live_session(session_id);

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
                "permissions": ["view-cache"],
                "organization": "test-org",
                "cache": "test-cache",
            }))
            .await;

        res.assert_status(axum::http::StatusCode::BAD_REQUEST);
        let body: serde_json::Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn create_cache_pinned_key_requires_manage_cache_members() {
    run(async {
        let session_id = gradient_core::types::SessionId::now_v7();
        let token = test_support::web::make_token(session_id);
        let session = test_support::web::live_session(session_id);

        // User has View role on the cache (lacks ManageCacheMembers) → 403.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![session.clone()]])
            .append_query_results([vec![session]])
            .append_query_results([vec![user()]])
            // Name-clash check returns empty (no existing key with that name)
            .append_query_results([Vec::<api::Model>::new()])
            // load_cache(ManageCacheMembers): cache lookup
            .append_query_results([vec![private_cache_row()]])
            // load_cache: member lookup
            .append_query_results([vec![view_cache_member()]])
            // load_cache: role lookup
            .append_query_results([vec![view_cache_role()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post("/api/v1/user/keys")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "my-cache-key",
                "permissions": ["view-cache"],
                "cache": "test-cache",
            }))
            .await;

        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn permissions_endpoint_returns_both_catalogues() {
    run(async {
        // GET /user/keys/permissions requires authentication.
        let session_id = gradient_core::types::SessionId::now_v7();
        let token = test_support::web::make_token(session_id);
        let session = test_support::web::live_session(session_id);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![session.clone()]])
            .append_query_results([vec![session]])
            .append_query_results([vec![user()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/user/keys/permissions")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["error"], false);
        assert!(body["message"]["available_permissions"].is_array());
        assert!(body["message"]["availableCache"].is_array());
        let cache_perms = body["message"]["availableCache"].as_array().unwrap();
        assert!(!cache_perms.is_empty(), "availableCache must not be empty");
        let ids: Vec<&str> = cache_perms
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"viewCache"), "must include viewCache");
        assert!(ids.contains(&"writeStore"), "must include writeStore");
    });
}
