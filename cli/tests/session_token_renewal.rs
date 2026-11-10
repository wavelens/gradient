/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use assert_cmd::Command;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DAY: u64 = 86_400;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn build_unsigned_jwt_with_issued_at_and_expiry(iat: u64, exp: u64) -> String {
    let payload = json!({ "iat": iat, "exp": exp, "id": "00000000-0000-0000-0000-0000000000b1", "jti": "[uuid1]" });
    format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(payload.to_string())
    )
}

async fn start_wiremock_server_for_session_token_renewal_with_expected_call_count(
    expected_renew_calls: u64,
) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": "ok"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/session/renew"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": false, "message": "renewed-token"})),
        )
        .expect(expected_renew_calls)
        .mount(&server)
        .await;
    server
}

fn home_with_saved_token(server: &MockServer, token: &str) -> TempDir {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("gradient");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("accounts.toml"),
        format!(
            "default_account = 'home'\n[accounts.home]\nserver = '{}'\nauth_token = '{token}'\n",
            server.uri()
        ),
    )
    .unwrap();
    home
}

fn gradient_status(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("gradient").unwrap();
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_ACCOUNT")
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_TOKEN")
        .args(["--json", "status"]);
    cmd
}

fn saved_accounts(home: &TempDir) -> String {
    fs::read_to_string(home.path().join("gradient/accounts.toml")).unwrap()
}

#[tokio::test]
async fn renews_saved_session_token_past_half_lifetime_and_writes_new_token_to_accounts_toml() {
    let server = start_wiremock_server_for_session_token_renewal_with_expected_call_count(0).await;
    let token = build_unsigned_jwt_with_issued_at_and_expiry(now() - 20 * DAY, now() + 10 * DAY);
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/session/renew"))
        .and(header("Authorization", format!("Bearer {token}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": false, "message": "renewed-token"})),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let home = home_with_saved_token(&server, &token);

    gradient_status(&home).assert().success();

    let accounts = saved_accounts(&home);
    assert!(accounts.contains("renewed-token"), "{accounts}");
    assert!(!accounts.contains(&token), "{accounts}");
}

#[tokio::test]
async fn token_from_gradient_token_env_var_is_never_renewed() {
    let server = start_wiremock_server_for_session_token_renewal_with_expected_call_count(0).await;
    let stale = build_unsigned_jwt_with_issued_at_and_expiry(now() - 20 * DAY, now() + 10 * DAY);
    let home = home_with_saved_token(&server, "saved-token");

    gradient_status(&home)
        .env("GRADIENT_TOKEN", &stale)
        .assert()
        .success();

    assert!(saved_accounts(&home).contains("saved-token"));
}

#[tokio::test]
async fn saved_session_token_in_first_half_of_lifetime_is_not_renewed() {
    let server = start_wiremock_server_for_session_token_renewal_with_expected_call_count(0).await;
    let fresh = build_unsigned_jwt_with_issued_at_and_expiry(now() - DAY, now() + 29 * DAY);
    let home = home_with_saved_token(&server, &fresh);

    gradient_status(&home).assert().success();

    assert!(saved_accounts(&home).contains(&fresh));
}
