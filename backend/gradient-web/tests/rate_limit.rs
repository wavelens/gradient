/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_test_support::state::TestState;

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
