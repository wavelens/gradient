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
use predicates::prelude::*;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_file_to_temp_gradient_config_dir(home: &TempDir, file_name: &str, body: &str) {
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join(file_name), body).unwrap();
}

fn write_config(home: &TempDir, body: &str) {
    write_file_to_temp_gradient_config_dir(home, "config.toml", body);
}

fn write_saved_accounts_file_in_temp_home(home: &TempDir, body: &str) {
    write_file_to_temp_gradient_config_dir(home, "accounts.toml", body);
}

fn saved_accounts_file_path_in_temp_home(home: &TempDir) -> std::path::PathBuf {
    home.path().join("gradient/accounts.toml")
}

fn gradient(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("gradient").unwrap();
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_ACCOUNT")
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_TOKEN");
    cmd
}

fn accounts(home: &TempDir) -> Vec<Value> {
    let output = gradient(home)
        .args(["--json", "account", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    envelope["message"].as_array().unwrap().clone()
}

#[test]
fn a_single_login_config_shows_up_as_the_default_account() {
    let home = TempDir::new().unwrap();
    write_config(&home, "Server = 'http://a.invalid'\nAuthToken = 't'\n");

    let rows = accounts(&home);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], json!("default"));
    assert_eq!(rows[0]["active"], json!(true));
    assert_eq!(rows[0]["logged_in"], json!(true));
}

#[test]
fn accounts_keep_separate_servers_and_switch_with_use() {
    let home = TempDir::new().unwrap();
    write_saved_accounts_file_in_temp_home(
        &home,
        "default_account = 'home'\n[accounts.home]\nserver = 'http://a.invalid'\n[accounts.work]\nserver = 'http://w.invalid'\n",
    );

    gradient(&home)
        .args(["config", "server"])
        .assert()
        .stdout(predicate::str::contains("http://a.invalid"));
    gradient(&home)
        .args(["--account", "work", "config", "server"])
        .assert()
        .stdout(predicate::str::contains("http://w.invalid"));

    gradient(&home)
        .args(["account", "switch", "work"])
        .assert()
        .success();
    gradient(&home)
        .args(["config", "server"])
        .assert()
        .stdout(predicate::str::contains("http://w.invalid"));
}

#[test]
fn switch_without_a_name_needs_a_terminal() {
    let home = TempDir::new().unwrap();
    write_config(&home, "Server = 'http://a.invalid'\n");

    gradient(&home)
        .args(["account", "switch"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn removing_the_default_account_leaves_no_default() {
    let home = TempDir::new().unwrap();
    write_saved_accounts_file_in_temp_home(
        &home,
        "default_account = 'work'\n[accounts.default]\nserver = 'http://d.invalid'\n[accounts.work]\nserver = 'http://w.invalid'\n",
    );

    gradient(&home)
        .args(["account", "remove", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gradient account switch"));

    let rows = accounts(&home);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["active"], json!(false));
    for args in [
        &["config", "server"][..],
        &["status"],
        &["login", "http://x.invalid"],
    ] {
        gradient(&home)
            .args(args)
            .assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("gradient account switch"));
    }
    gradient(&home)
        .args(["--account", "default", "config", "server"])
        .assert()
        .success()
        .stdout(predicate::str::contains("http://d.invalid"));
}

#[test]
fn config_writes_never_create_a_mistyped_account() {
    let home = TempDir::new().unwrap();
    write_config(&home, "Server = 'http://a.invalid'\n");

    gradient(&home)
        .args(["--account", "wrok", "config", "server", "http://w.invalid"])
        .assert()
        .failure()
        .code(2);
    assert_eq!(accounts(&home).len(), 1);
}

#[test]
fn a_corrupt_config_file_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    write_config(&home, "this is = = not toml");

    gradient(&home)
        .args(["config", "server"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Invalid config file"));
}

async fn login_server(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/basic/login"))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_json(json!({"error": status != 200, "message": "new-token"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("Authorization", "Bearer new-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "id": "u", "username": "neo", "name": "N", "email": "n@example.invalid",
                "superuser": false, "can_create_cache": false
            }
        })))
        .mount(&server)
        .await;
    server
}

fn login(home: &TempDir, server: &MockServer) -> assert_cmd::assert::Assert {
    gradient(home)
        .args(["--json", "login", &server.uri(), "-u", "neo", "-p", "pw"])
        .assert()
}

#[tokio::test]
async fn logging_in_to_another_server_resets_the_account() {
    let server = login_server(200).await;
    let home = TempDir::new().unwrap();
    write_config(
        &home,
        "Server = 'http://old.invalid'\nAuthToken = 'old-token'\nSelectedProject = 'p'\n",
    );

    login(&home, &server).success();

    let config = fs::read_to_string(saved_accounts_file_path_in_temp_home(&home)).unwrap();
    assert!(config.contains("new-token"), "{config}");
    assert!(config.contains("neo"), "{config}");
    assert!(!config.contains("selected_project"), "{config}");
    let legacy = fs::read_to_string(home.path().join("gradient/config.toml")).unwrap();
    assert!(
        legacy.contains("old-token"),
        "an older CLI must still read config.toml: {legacy}"
    );
}

#[tokio::test]
async fn a_failed_login_keeps_the_saved_account() {
    let server = login_server(401).await;
    let home = TempDir::new().unwrap();
    write_config(
        &home,
        "Server = 'http://old.invalid'\nAuthToken = 'old-token'\nSelectedProject = 'p'\n",
    );

    login(&home, &server).failure();
    assert!(!saved_accounts_file_path_in_temp_home(&home).exists());
}

#[test]
fn an_unknown_account_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    write_config(&home, "Server = 'http://a.invalid'\n");

    gradient(&home)
        .args(["--account", "nope", "status"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("'nope' does not exist"));
    gradient(&home)
        .args(["account", "switch", "nope"])
        .assert()
        .failure()
        .code(2);
}

#[tokio::test]
async fn server_and_token_come_from_the_environment_without_a_config_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("Authorization", "Bearer env-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "id": "00000000-0000-0000-0000-0000000000b1",
                "username": "ci",
                "name": "CI",
                "email": "ci@example.invalid",
                "superuser": false,
                "can_create_cache": false
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();

    gradient(&home)
        .env("GRADIENT_SERVER", server.uri())
        .env("GRADIENT_TOKEN", "env-token")
        .args(["info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Username: ci"));
    assert!(!saved_accounts_file_path_in_temp_home(&home).exists());
}

#[test]
fn the_config_file_is_private() {
    let home = TempDir::new().unwrap();

    gradient(&home)
        .args(["config", "authtoken", "secret"])
        .assert()
        .success();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(saved_accounts_file_path_in_temp_home(&home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn json_mode_never_opens_an_editor_for_missing_fields() {
    let home = TempDir::new().unwrap();
    write_config(
        &home,
        "Server = 'http://a.invalid'\nAuthToken = 't'\nSelectedProject = 'p'\n",
    );

    gradient(&home)
        .env("EDITOR", "false")
        .args(["--json", "project", "create", "--name", "x"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("\"error\":true"));
}

#[test]
fn concurrent_config_writes_keep_every_update() {
    let home = TempDir::new().unwrap();
    let names: Vec<String> = (0..8).map(|i| format!("a{i}")).collect();
    let seeded: String = names
        .iter()
        .map(|name| format!("[accounts.{name}]\n"))
        .collect();
    write_saved_accounts_file_in_temp_home(&home, &format!("default_account = 'a0'\n{seeded}"));

    let children: Vec<_> = names
        .iter()
        .map(|name| {
            let server = format!("http://{name}.invalid");
            std::process::Command::new(assert_cmd::cargo::cargo_bin("gradient"))
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path())
                .args(["--account", name, "config", "server", &server])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let saved = fs::read_to_string(saved_accounts_file_path_in_temp_home(&home)).unwrap();
    for name in &names {
        assert!(saved.contains(&format!("http://{name}.invalid")), "{saved}");
    }
}
