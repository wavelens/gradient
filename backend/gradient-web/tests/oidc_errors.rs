/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_test_support::state::TestState;
use gradient_types::cli::OidcArgs;
use serde_json::Value;
use uuid::Uuid;

/// `127.0.0.1:1` is reserved and is refusing connections immediately.
fn server_with_broken_oidc() -> TestServer {
    let tmp = std::env::temp_dir();
    let suffix = Uuid::now_v7();

    let jwt_path = tmp.join(format!("gradient-test-jwt-{}", suffix));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    let client_secret_path = tmp.join(format!("gradient-test-oidc-secret-{}", suffix));
    std::fs::write(&client_secret_path, "test-client-secret").expect("write client secret file");

    TestState::new()
        .cli(|cli| {
            cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned();
            cli.oidc = OidcArgs {
                enable: true,
                required: false,
                client_id: Some("test-client".into()),
                client_secret_file: Some(client_secret_path.to_string_lossy().into_owned()),
                discovery_url: Some("http://127.0.0.1:1/oidc".into()),
                ..Default::default()
            };
        })
        .server()
}

const LEAK_MARKERS: &[&str] = &[
    "fetch OIDC metadata",
    "parse OIDC metadata",
    "tcp connect",
    "connection refused",
    "127.0.0.1:1",
    "reqwest",
    "os error",
];

fn assert_no_leak(message: &str) {
    for marker in LEAK_MARKERS {
        assert!(
            !message.to_lowercase().contains(&marker.to_lowercase()),
            "response body leaks internal error detail {:?}: full message = {:?}",
            marker,
            message,
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn oidc_login_get_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s.get("/api/v1/auth/oidc/login").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_authorize_post_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s.post("/api/v1/auth/oauth/authorize").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_authorize_get_callback_does_not_leak_idp_error() {
    let s = server_with_broken_oidc();
    let res = s
        .get("/api/v1/auth/oauth/authorize?code=abc&state=xyz")
        .await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    let msg = body["message"].as_str().expect("message string");
    assert_no_leak(msg);
}
