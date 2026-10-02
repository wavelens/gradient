/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_test_support::web::{make_test_server, make_test_server_configured};
use gradient_types::CreatePermission;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

#[tokio::test]
async fn config_defaults_to_everyone() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server(db.into_connection());

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["create_project"], "everyone");
    assert_eq!(body["message"]["create_cache"], "everyone");
    assert_eq!(body["message"]["github_app_enabled"], false);
}

#[tokio::test]
async fn config_reports_a_configured_github_app() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.github_app.id = Some(1);
        cli.github_app.private_key_file = Some("/run/key.pem".into());
        cli.github_app.webhook_secret_file = Some("/run/secret".into());
    });

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["github_app_enabled"], true);
}

#[tokio::test]
async fn config_reflects_configured_permissions() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_project = CreatePermission::None;
        cli.permissions.create_cache = CreatePermission::Superusers;
    });

    let res = server.get("/api/v1/config").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["create_project"], "none");
    assert_eq!(body["message"]["create_cache"], "superusers");
}

#[tokio::test]
async fn config_reports_the_ssh_port_when_enabled() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.ssh.enable = true;
        cli.ssh.port = 2200;
    });

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["ssh_enabled"], true);
    assert_eq!(body["message"]["ssh_port"], 2200);
}

#[tokio::test]
async fn config_hides_the_ssh_port_when_disabled() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server(db.into_connection());

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["ssh_enabled"], false);
    assert!(body["message"]["ssh_port"].is_null());
}

#[tokio::test]
async fn config_offers_gradient_ci_by_default() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server(db.into_connection());

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["gradient_ci_enabled"], true);
    assert_eq!(
        body["message"]["gradient_ci_url"],
        "https://servers.gradient.ci"
    );
}

#[tokio::test]
async fn config_reports_gradient_ci_turned_off() {
    let db = MockDatabase::new(DatabaseBackend::Postgres);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.gradient_ci.enable = false;
    });

    let body: Value = server.get("/api/v1/config").await.json();
    assert_eq!(body["message"]["gradient_ci_enabled"], false);
}
