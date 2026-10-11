/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::mock::mock_db;
use gradient_test_support::state::TestState;
use gradient_test_support::web::make_test_server;
use gradient_types::CreatePermission;
use serde_json::Value;

#[tokio::test]
async fn config_defaults_to_everyone() {
    let db = mock_db();
    let server = make_test_server(db.into_connection());

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["create_project"], "everyone");
    assert_eq!(body["message"]["create_team"], "everyone");
    assert_eq!(body["message"]["create_cache"], "everyone");
    assert_eq!(body["message"]["github_app_enabled"], false);
}

#[tokio::test]
async fn config_reports_a_configured_github_app() {
    let db = mock_db();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.github_app.id = std::num::NonZeroU64::new(1);
            cli.github_app.private_key_file = Some("/run/key.pem".into());
            cli.github_app.webhook_secret_file = Some("/run/secret".into());
        })
        .server();

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["github_app_enabled"], true);
}

#[tokio::test]
async fn config_reflects_configured_permissions() {
    let db = mock_db();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_project = CreatePermission::None;
            cli.permissions.create_cache = CreatePermission::Superusers;
        })
        .server();

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["create_project"], "none");
    assert_eq!(body["message"]["create_cache"], "superusers");
}

#[tokio::test]
async fn config_reports_the_ssh_port_when_enabled() {
    let db = mock_db();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.ssh.enable = true;
            cli.ssh.port = 2200;
        })
        .server();

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["ssh_enabled"], true);
    assert_eq!(body["message"]["ssh_port"], 2200);
}

#[tokio::test]
async fn config_hides_the_ssh_port_when_disabled() {
    let db = mock_db();
    let server = make_test_server(db.into_connection());

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["ssh_enabled"], false);
    assert!(body["message"]["ssh_port"].is_null());
}

#[tokio::test]
async fn config_reports_gradient_ci_turned_off() {
    let db = mock_db();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.gradient_ci.enable = false;
        })
        .server();

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["gradient_ci_enabled"], false);
}
