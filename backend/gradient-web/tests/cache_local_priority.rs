/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use gradient_fixtures::mock::mock_db;
use gradient_test_support::state::TestState;
use gradient_types::ids::*;
use std::net::SocketAddr;
use uuid::Uuid;

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap())
}

fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000002").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn cache_row(local_priority: Option<i32>) -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 40,
        local_priority,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn build_server(cache: gradient_entity::cache::Model, peer: &str) -> TestServer {
    let db = mock_db()
        .append_query_results([vec![cache]])
        .into_connection();

    let state = TestState::new().web_db(db).build();

    let peer_addr: SocketAddr = format!("{peer}:0").parse().expect("valid peer addr");
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(peer_addr));
    TestServer::new(router)
}

#[tokio::test]
async fn local_priority_swapped_when_xff_in_local_ips() {
    let server = build_server(cache_row(Some(10)), "127.0.0.1");
    let resp = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("x-forwarded-for", "10.0.0.5")
        .await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("Priority: 10"),
        "expected Priority: 10, got:\n{body}"
    );
}

#[tokio::test]
async fn local_priority_not_swapped_for_non_local_xff() {
    let server = build_server(cache_row(Some(10)), "127.0.0.1");
    let resp = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("x-forwarded-for", "8.8.8.8")
        .await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("Priority: 40"),
        "expected Priority: 40, got:\n{body}"
    );
}

#[tokio::test]
async fn local_priority_null_always_uses_default() {
    let server = build_server(cache_row(None), "127.0.0.1");
    let resp = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("x-forwarded-for", "10.0.0.5")
        .await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("Priority: 40"),
        "expected Priority: 40, got:\n{body}"
    );
}

#[tokio::test]
async fn local_priority_zero_treated_as_disabled() {
    let server = build_server(cache_row(Some(0)), "127.0.0.1");
    let resp = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("x-forwarded-for", "10.0.0.5")
        .await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("Priority: 40"),
        "expected Priority: 40, got:\n{body}"
    );
}

#[tokio::test]
async fn untrusted_peer_xff_is_ignored_for_priority_decision() {
    let server = build_server(cache_row(Some(10)), "203.0.113.5");
    let resp = server
        .get("/cache/test-cache/nix-cache-info")
        .add_header("x-forwarded-for", "10.0.0.5")
        .await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("Priority: 40"),
        "expected Priority: 40, got:\n{body}"
    );
}
