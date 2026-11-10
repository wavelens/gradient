/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::connect_info::MockConnectInfo;
use gradient_entity::api;
use gradient_entity::ids::ApiId;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::user;
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use gradient_web::authorization::hash_api_key;
use sea_orm::MockDatabase;
use std::net::SocketAddr;
use std::num::NonZeroU32;

#[tokio::test]
async fn auth_tier_throttles_burst() {
    let server = TestState::new().server();

    let login = || {
        server
            .post("/api/v1/auth/basic/login")
            .json(&serde_json::json!({"loginname": "x", "password": "x"}))
    };

    for i in 1..=5 {
        let status = login().await.status_code();
        assert_ne!(status, 429, "request {i} unexpectedly throttled");
    }

    let throttled = login().await.status_code();
    assert_eq!(
        throttled, 429,
        "6th burst request should be 429, got {throttled:?}"
    );
}

#[tokio::test]
async fn username_availability_allows_fifteen_rapid_checks_before_rate_limit_response() {
    let server = TestState::new().server();
    let check = || {
        server
            .post("/api/v1/auth/check-username")
            .json(&serde_json::json!({"username": "x"}))
    };

    for i in 1..=15 {
        let status = check().await.status_code();
        assert_eq!(status, 200, "check {i} unexpectedly refused: {status:?}");
    }

    assert_eq!(check().await.status_code(), 429);
    assert_ne!(
        server
            .post("/api/v1/auth/basic/login")
            .json(&serde_json::json!({"loginname": "x", "password": "x"}))
            .await
            .status_code(),
        429,
        "username checks must not use up the sign-in budget"
    );
}

#[tokio::test]
async fn cache_tier_does_not_throttle_moderate_burst() {
    let server = TestState::new().server();

    for i in 1..=50 {
        let resp = server.get("/cache/missing-cache/nix-cache-info").await;
        assert_ne!(
            resp.status_code(),
            429,
            "cache request {} unexpectedly throttled",
            i
        );
    }
}

#[tokio::test]
async fn cache_upload_tier_outlasts_the_api_burst() {
    let server = TestState::new().server();

    for i in 1..=300 {
        let resp = server
            .post("/api/v1/caches/mycache/nars/abc/finalize")
            .json(&serde_json::json!({}))
            .await;
        assert_ne!(
            resp.status_code(),
            429,
            "upload request {} unexpectedly throttled",
            i
        );
    }
}

#[tokio::test]
async fn cache_proto_tier_does_not_throttle_burst() {
    let server = TestState::new().server();

    for i in 1..=250 {
        let resp = server.get("/cache/missing-cache/proto").await;
        assert_ne!(
            resp.status_code(),
            429,
            "cache proto request {} unexpectedly throttled",
            i
        );
    }
}

fn cache_server_with_burst(burst: u32, peer: &str) -> axum_test::TestServer {
    let state = TestState::new()
        .config(move |config| {
            config.http.cache_requests_per_second = NonZeroU32::MIN;
            config.http.cache_request_burst = NonZeroU32::new(burst).expect("a non-zero burst");
        })
        .build();
    let peer: SocketAddr = format!("{peer}:0").parse().expect("a peer address");
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(peer));
    axum_test::TestServer::new(router)
}

async fn cache_status(server: &axum_test::TestServer, forwarded_for: &str) -> u16 {
    server
        .get("/cache/missing-cache/nix-cache-info")
        .add_header("x-forwarded-for", forwarded_for)
        .await
        .status_code()
        .as_u16()
}

#[tokio::test]
async fn cache_tier_throttles_after_the_configured_burst() {
    let server = cache_server_with_burst(3, "127.0.0.1");

    for i in 1..=3 {
        assert_ne!(
            cache_status(&server, "203.0.113.1").await,
            429,
            "request {i}"
        );
    }
    assert_eq!(cache_status(&server, "203.0.113.1").await, 429);
}

#[tokio::test]
async fn clients_behind_a_trusted_proxy_have_their_own_cache_budget() {
    let server = cache_server_with_burst(1, "127.0.0.1");

    assert_ne!(cache_status(&server, "203.0.113.1").await, 429);
    assert_ne!(cache_status(&server, "203.0.113.2").await, 429);
    assert_eq!(cache_status(&server, "203.0.113.1").await, 429);
}

#[tokio::test]
async fn signed_in_users_do_not_share_the_budget_of_their_ip() {
    let (web_db, bearer) = signed_in();
    let state = TestState::new().web_db(web_db.into_connection()).build();
    let peer: SocketAddr = "198.51.100.7:0".parse().expect("a peer address");
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(peer));
    let server = axum_test::TestServer::new(router);

    for i in 1..=150 {
        let status = server.get("/api/v1/health").await.status_code();
        assert_ne!(status, 429, "anonymous request {i}");
    }
    assert_eq!(server.get("/api/v1/health").await.status_code(), 429);

    let signed_in = server
        .get("/api/v1/user")
        .add_header("authorization", bearer)
        .await;
    assert_ne!(signed_in.status_code(), 429);
}

#[tokio::test]
async fn an_untrusted_peer_cannot_dodge_the_cache_budget_with_a_forwarded_header() {
    let server = cache_server_with_burst(1, "198.51.100.7");

    assert_ne!(cache_status(&server, "203.0.113.1").await, 429);
    assert_eq!(cache_status(&server, "203.0.113.2").await, 429);
}

fn api_key_of_the_same_user(raw: &str) -> api::Model {
    let now = chrono::Utc::now().naive_utc();
    api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: raw[..1].into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission: i64::MAX,
        ..Default::default()
    }
}

fn with_requests_of_key(db: MockDatabase, key: &api::Model, requests: usize) -> MockDatabase {
    let first = db
        .append_query_results([vec![key.clone()]])
        .append_exec_results([exec_ok()])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![user()]]);
    (1..requests).fold(first, |db, _| {
        db.append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
    })
}

#[tokio::test]
async fn api_keys_of_one_user_do_not_share_a_budget() {
    let busy_raw = "a".repeat(64);
    let other_raw = "b".repeat(64);
    let busy = api_key_of_the_same_user(&busy_raw);
    let other = api_key_of_the_same_user(&other_raw);
    let db = with_requests_of_key(mock_db(), &busy, 151);
    let db = with_requests_of_key(db, &other, 1);
    let server = TestState::new().web_db(db.into_connection()).server();
    let get_user = |raw: &str| {
        server
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer GRAD{raw}"))
    };

    for i in 1..=150 {
        assert_eq!(get_user(&busy_raw).await.status_code(), 200, "request {i}");
    }
    assert_eq!(get_user(&busy_raw).await.status_code(), 429);
    assert_eq!(get_user(&other_raw).await.status_code(), 200);
}
