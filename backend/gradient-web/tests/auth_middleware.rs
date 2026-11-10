/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::ids::SessionId;
use gradient_fixtures::mock::mock_db;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, make_token};
use sea_orm::DbErr;
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
    let res = s.get("/api/v1/user/search").await;
    res.assert_status_forbidden();
    assert_envelope(&res.json::<Value>(), "Authorization header not found");
}

#[tokio::test(flavor = "multi_thread")]
async fn get_user_without_credentials_returns_200_envelope() {
    let res = server().get("/api/v1/user").await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        serde_json::json!({ "error": false, "message": null })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_bearer_returns_403_envelope() {
    let s = server();
    let res = s
        .get("/api/v1/user/search")
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
    assert_envelope(
        &res.json::<Value>(),
        "Your session has ended or the API key is no longer valid. Sign in again.",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_jwt_token_cookie_on_optional_auth_route_returns_401_and_clears_cookie() {
    let s = server();
    let res = s
        .get("/api/v1/projects/x")
        .add_header("cookie", "jwt_token=not-a-real-jwt")
        .await;
    res.assert_status_unauthorized();
    let set_cookie = res.header("set-cookie");
    let set_cookie = set_cookie.to_str().expect("ascii set-cookie");
    assert!(set_cookie.starts_with("jwt_token=;"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_session_lookup_answers_500_and_keeps_the_session_cookie() {
    for url in ["/api/v1/projects/x", "/api/v1/user/search"] {
        let db = mock_db()
            .append_query_errors([DbErr::Custom("pool timed out".into())])
            .into_connection();
        let cookie = format!("jwt_token={}", make_token(SessionId::now_v7()));

        let res = make_test_server(db)
            .get(url)
            .add_header("cookie", cookie)
            .await;

        assert_eq!(
            res.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{url}"
        );
        assert!(res.maybe_header("set-cookie").is_none(), "{url}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_bearer_token_on_optionally_authenticated_route_returns_401_without_set_cookie() {
    let s = server();
    let res = s
        .get("/api/v1/projects/x")
        .add_header("authorization", "Bearer not-a-real-jwt")
        .await;
    res.assert_status_unauthorized();
    assert!(res.maybe_header("set-cookie").is_none());
}
