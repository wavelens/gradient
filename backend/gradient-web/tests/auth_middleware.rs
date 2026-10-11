/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_test_support::state::TestState;
use serde_json::Value;
use uuid::Uuid;

/// `load_secret` is calling `process::exit(1)` on a missing file. The JWT secret must exist on
/// disk, or the test process would die before any assertion.
fn server() -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-test-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    TestState::new()
        .cli(|cli| cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned())
        .server()
}

fn assert_envelope(body: &Value, expected_message: &str) {
    assert_eq!(body["error"], Value::Bool(true), "error flag must be true");
    assert_eq!(
        body["message"],
        Value::String(expected_message.to_string()),
        "message must match"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_auth_header_returns_403_envelope() {
    let s = server();
    let res = s.get("/api/v1/user").await;
    res.assert_status_forbidden();
    assert_envelope(&res.json::<Value>(), "Authorization header not found");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_bearer_returns_403_envelope() {
    let s = server();
    let res = s
        .get("/api/v1/user")
        .add_header("authorization", "NotBearer xyz")
        .await;
    res.assert_status_forbidden();
    assert_envelope(&res.json::<Value>(), "Invalid Authorization header");
}

#[tokio::test(flavor = "multi_thread")]
async fn undecodable_token_returns_401_envelope() {
    let s = server();
    let res = s
        .get("/api/v1/user")
        .add_header("authorization", "Bearer not-a-real-jwt")
        .await;
    res.assert_status_unauthorized();
    assert_envelope(&res.json::<Value>(), "Unable to decode token");
}
