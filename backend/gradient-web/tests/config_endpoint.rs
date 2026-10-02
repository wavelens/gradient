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
