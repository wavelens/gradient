/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::connect_info::MockConnectInfo;
use gradient_test_support::state::TestState;
use std::net::SocketAddr;
use std::num::NonZeroU32;

#[tokio::test]
async fn auth_tier_throttles_burst() {
    let server = TestState::new().server();

    for i in 1..=5 {
        let resp = server
            .post("/api/v1/auth/check-username")
            .json(&serde_json::json!({"username": "x"}))
            .await;
        assert_eq!(
            resp.status_code(),
            200,
            "request {} unexpectedly throttled: {:?}",
            i,
            resp.status_code()
        );
    }

    let throttled = server
        .post("/api/v1/auth/check-username")
        .json(&serde_json::json!({"username": "x"}))
        .await;
    assert_eq!(
        throttled.status_code(),
        429,
        "6th burst request should be 429, got {:?}",
        throttled.status_code()
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
async fn an_untrusted_peer_cannot_dodge_the_cache_budget_with_a_forwarded_header() {
    let server = cache_server_with_burst(1, "198.51.100.7");

    assert_ne!(cache_status(&server, "203.0.113.1").await, 429);
    assert_eq!(cache_status(&server, "203.0.113.2").await, 429);
}
