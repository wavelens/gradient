/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_config(home: &TempDir, body: &str) {
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), body).unwrap();
}

async fn orgs_mock(names: &[&str]) -> MockServer {
    let server = MockServer::start().await;
    let items: Vec<_> = names
        .iter()
        .map(|n| serde_json::json!({"id": format!("id-{n}"), "name": n}))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": {"items": items, "total": items.len(), "page": 1, "per_page": 20}
        })))
        .mount(&server)
        .await;
    server
}

#[test]
fn organization_select_without_login_is_rejected() {
    let home = TempDir::new().unwrap();
    write_config(&home, "Server = 'http://localhost:1'\n");

    Command::cargo_bin("gradient")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .args(["organization", "select", "sandro"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("Not logged in"))
        .stderr(predicate::str::contains("gradient login"));
}

#[tokio::test]
async fn organization_select_rejects_non_member() {
    let server = orgs_mock(&["other"]).await;
    let home = TempDir::new().unwrap();
    write_config(
        &home,
        &format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    );

    Command::cargo_bin("gradient")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .args(["organization", "select", "sandro"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("not a member"));
}

#[tokio::test]
async fn organization_select_accepts_member() {
    let server = orgs_mock(&["sandro"]).await;
    let home = TempDir::new().unwrap();
    write_config(
        &home,
        &format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    );

    Command::cargo_bin("gradient")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .args(["organization", "select", "sandro"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Organization selected"));
}
