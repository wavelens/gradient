/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "Test scaffolding: Fixture helpers that cannot construct their own value fail the test loudly."
)]

use axum::http::StatusCode;
use base64::Engine;
use gradient_entity::{api, cache, ids::*};
use gradient_fixtures::ids::{cache_id, project_id, user_id};
use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::{cache_member, cache_view_role, user};
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::consts::BASE_CACHE_ROLE_VIEW_ID;
use gradient_types::permissions::{CachePermission, Permission, cache_mask_from, mask_from};
use sea_orm::MockDatabase;
use serde_json::json;
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

fn key_row(raw: &str, permission: i64, cache: Option<CacheId>) -> api::Model {
    let now = chrono::Utc::now().naive_utc();
    api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "key".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission,
        cache,
        ..Default::default()
    }
}

fn view_project_key(raw: &str) -> api::Model {
    key_row(raw, mask_from(&[Permission::ViewProject]), None)
}

fn with_key(db: MockDatabase, key: &api::Model) -> MockDatabase {
    db.append_query_results([vec![key.clone()]])
        .append_exec_results([exec_ok()])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![user()]])
}

fn private_cache() -> cache::Model {
    gradient_fixtures::rows::cache()
}

fn bearer_key(raw: &str) -> String {
    format!("Bearer GRAD{raw}")
}

fn basic(password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("nix:{password}"));
    format!("Basic {encoded}")
}

async fn refused_with_key(
    db: MockDatabase,
    raw: &str,
    request: impl FnOnce(&axum_test::TestServer) -> axum_test::TestRequest,
) -> StatusCode {
    let server = make_test_server(db.into_connection());
    request(&server)
        .add_header("authorization", bearer_key(raw))
        .await
        .status_code()
}

#[tokio::test]
async fn any_project_key_cannot_create_a_cache() {
    let raw = "a".repeat(64);
    let db = with_key(mock_db(), &view_project_key(&raw));
    let status = refused_with_key(db, &raw, |s| {
        s.put("/api/v1/caches").json(&json!({
            "name": "new-cache",
            "display_name": "New",
            "description": "",
            "priority": 30,
        }))
    })
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn any_project_key_cannot_change_or_delete_a_member_cache() {
    for method in ["PATCH", "DELETE"] {
        let raw = "b".repeat(64);
        let db = with_key(mock_db(), &key_row(&raw, i64::MAX, None))
            .append_query_results([vec![private_cache()]]);
        let status = refused_with_key(db, &raw, |s| match method {
            "PATCH" => s
                .patch("/api/v1/caches/test-cache")
                .json(&json!({ "display_name": "Renamed" })),
            _ => s.delete("/api/v1/caches/test-cache"),
        })
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
    }
}

#[tokio::test]
async fn any_project_key_cannot_read_a_private_cache_without_a_grant() {
    let raw = "c".repeat(64);
    let db = with_key(mock_db(), &view_project_key(&raw))
        .append_query_results([vec![private_cache()]])
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()]);
    let status = refused_with_key(db, &raw, |s| s.get("/api/v1/caches/test-cache")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_keys_cannot_create_list_change_or_delete_teams() {
    let any_project = "d".repeat(64);
    let cache_scope = "e".repeat(64);
    let keys = [
        (any_project.clone(), key_row(&any_project, i64::MAX, None)),
        (
            cache_scope.clone(),
            key_row(
                &cache_scope,
                cache_mask_from(CachePermission::ALL),
                Some(cache_id()),
            ),
        ),
    ];
    for (raw, key) in keys {
        for method in ["PUT", "GET", "PATCH", "DELETE"] {
            let db = with_key(mock_db(), &key);
            let status = refused_with_key(db, &raw, |s| match method {
                "PUT" => s
                    .put("/api/v1/teams")
                    .json(&json!({ "name": "platform", "display_name": "Platform" })),
                "GET" => s.get("/api/v1/teams"),
                "PATCH" => s
                    .patch("/api/v1/teams/platform")
                    .json(&json!({ "display_name": "Renamed" })),
                _ => s.delete("/api/v1/teams/platform"),
            })
            .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} with {:?}",
                key.cache
            );
        }
    }
}

fn cache_scope_key(raw: &str) -> api::Model {
    key_row(raw, cache_mask_from(CachePermission::ALL), Some(cache_id()))
}

#[tokio::test]
async fn api_keys_cannot_create_a_project() {
    let any_project = "h".repeat(64);
    let cache_scope = "i".repeat(64);
    for (raw, key) in [
        (any_project.clone(), key_row(&any_project, i64::MAX, None)),
        (cache_scope.clone(), cache_scope_key(&cache_scope)),
    ] {
        let db = with_key(mock_db(), &key);
        let status = refused_with_key(db, &raw, |s| {
            s.put("/api/v1/projects").json(&json!({
                "name": "new-project",
                "display_name": "New",
                "description": "",
            }))
        })
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{:?}", key.cache);
    }
}

#[tokio::test]
async fn cache_scope_key_is_refused_outside_cache_routes() {
    let raw = "j".repeat(64);
    let key = cache_scope_key(&raw);
    for path in [
        "/api/v1/projects",
        "/api/v1/dashboard/tasks",
        "/api/v1/projects/test-project",
        "/api/v1/user/stars",
    ] {
        let db = with_key(mock_db(), &key);
        let status = refused_with_key(db, &raw, |s| s.get(path)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn project_scope_key_is_refused_on_routes_reading_all_projects() {
    let raw = "q".repeat(64);
    let key = api::Model {
        project: Some(project_id()),
        ..key_row(&raw, mask_from(Permission::ALL), None)
    };
    for path in [
        "/api/v1/dashboard/tasks",
        "/api/v1/search?q=nightly",
        "/api/v1/board/jobs/pending",
        "/api/v1/metrics/catalog",
    ] {
        let db = with_key(mock_db(), &key);
        let status = refused_with_key(db, &raw, |s| s.get(path)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}

async fn pull_status(db: MockDatabase, password: &str) -> StatusCode {
    let server = make_test_server(db.into_connection());
    server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("authorization", basic(password))
        .await
        .status_code()
}

#[tokio::test]
async fn any_project_key_cannot_pull_from_a_member_cache() {
    let raw = "f".repeat(64);
    let db = with_key(
        mock_db().append_query_results([vec![private_cache()]]),
        &view_project_key(&raw),
    )
    .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
    .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
    .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()]);
    assert_eq!(
        pull_status(db, &format!("GRAD{raw}")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn any_project_key_of_a_cache_member_is_told_to_use_a_cache_key() {
    let raw = "r".repeat(64);
    let db = with_key(
        mock_db().append_query_results([vec![private_cache()]]),
        &view_project_key(&raw),
    )
    .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
    .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
    .append_query_results([vec![cache_view_role()]]);
    let server = make_test_server(db.into_connection());
    let res = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("authorization", basic(&format!("GRAD{raw}")))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert!(res.text().contains("pin to this cache"), "{}", res.text());
}

#[tokio::test]
async fn cache_scope_key_pulls_from_its_cache() {
    let raw = "g".repeat(64);
    let key = key_row(
        &raw,
        cache_mask_from(&[CachePermission::ViewCache, CachePermission::ReadStore]),
        Some(cache_id()),
    );
    let db = with_key(
        mock_db().append_query_results([vec![private_cache()]]),
        &key,
    )
    .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
    .append_query_results([vec![cache_view_role()]]);
    assert_eq!(pull_status(db, &format!("GRAD{raw}")).await, StatusCode::OK);
}

#[tokio::test]
async fn session_token_pulls_from_a_member_cache() {
    let session_id = SessionId::now_v7();
    let db = with_auth(
        mock_db().append_query_results([vec![private_cache()]]),
        session_id,
    )
    .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
    .append_query_results([vec![cache_view_role()]]);
    assert_eq!(
        pull_status(db, &make_token(session_id)).await,
        StatusCode::OK
    );
}
