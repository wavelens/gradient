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
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn temp_home_with_wiremock_gradient_config_token_and_project(server: &MockServer) -> TempDir {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "Server = '{}'\nAuthToken = 'test-token'\nSelectedProject = 'proj'\n",
            server.uri()
        ),
    )
    .unwrap();
    home
}

fn run_gradient_with_temp_home_stdin_and_args(
    home: &TempDir,
    stdin: &str,
    args: &[&str],
) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("TMPDIR", home.path())
        .env_remove("GRADIENT_TOKEN")
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_ACCOUNT")
        .args(args)
        .write_stdin(stdin)
        .assert()
}

fn ok(message: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message}))
}

#[tokio::test]
async fn project_secret_set_reads_piped_stdin_and_never_prints_the_value() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/proj/secrets/TOKEN"))
        .and(body_json(json!({"value": "s3cret-value"})))
        .respond_with(ok(json!({
            "name": "TOKEN", "updated_at": "2026-01-01T00:00:00", "updated_by": "alice", "managed": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        "s3cret-value\n",
        &["--json", "project", "secret", "set", "TOKEN"],
    )
    .success()
    .stdout(predicate::str::contains("TOKEN").and(predicate::str::contains("s3cret").not()));
}

#[tokio::test]
async fn project_team_grant_add_sends_the_grant_and_prints_the_server_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/proj/teams"))
        .and(body_json(
            json!({"team": "ops", "role": "View", "users": true, "workers": false}),
        ))
        .respond_with(ok(json!("Request sent")))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        "",
        &[
            "project",
            "team-grant",
            "add",
            "ops",
            "--role",
            "View",
            "--users",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Request sent."));
}

#[tokio::test]
async fn logged_in_user_api_key_revoke_resolves_the_name_to_the_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/keys"))
        .respond_with(ok(json!([{
            "id": "00000000-0000-0000-0000-000000000001", "name": "ci", "managed": false, "permissions": [],
            "created_at": "2026-01-01T00:00:00", "allowed_ips": []
        }])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/user/keys/00000000-0000-0000-0000-000000000001/revoke",
        ))
        .respond_with(ok(json!("API-Key revoked")))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        "",
        &["--json", "logged-in-user", "api-keys", "revoke", "ci"],
    )
    .success()
    .stdout(predicate::str::contains("\"revoked\":true"));
}

#[tokio::test]
async fn logged_in_user_delete_account_refuses_json_without_password() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user"))
        .respond_with(ok(json!("User deleted")))
        .expect(0)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        "",
        &["--json", "logged-in-user", "delete-account"],
    )
    .code(2)
    .stdout(predicate::str::contains("--password"));
}

fn read_saved_accounts_file_text(home: &TempDir) -> String {
    fs::read_to_string(home.path().join("gradient/accounts.toml")).unwrap()
}

#[tokio::test]
async fn logout_revokes_the_token_on_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/logout"))
        .respond_with(ok(json!("Logged out")))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(&home, "", &["--json", "logout"])
        .success()
        .stdout(predicate::str::contains("\"revoked\":true"));
    assert!(!read_saved_accounts_file_text(&home).contains("test-token"));
}

#[tokio::test]
async fn logout_clears_the_token_locally_when_the_server_revoke_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/logout"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(&home, "", &["--json", "logout"])
        .success()
        .stdout(predicate::str::contains("\"revoked\":false"));
    assert!(!read_saved_accounts_file_text(&home).contains("test-token"));
}

fn build_mcp_stdin_json_rpc_frames_for_initialize_initialized_and_run_gradient_command_call(
    args: Value,
) -> String {
    [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "gradient-test", "version": "0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "run_gradient_command", "arguments": {"args": args}}
        }),
    ]
    .iter()
    .map(|frame| format!("{frame}\n"))
    .collect()
}

#[tokio::test]
async fn mcp_refuses_secret_writes_even_with_flags_before_the_subcommand() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ok(json!(true)))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    for args in [
        json!([
            "project",
            "--project",
            "proj",
            "secret",
            "set",
            "K",
            "--value",
            "v"
        ]),
        json!(["project", "secret", "delete", "K"]),
        json!([
            "project",
            "webhook",
            "create",
            "--name",
            "hook",
            "--url",
            "https://example.com"
        ]),
        json!([
            "team",
            "webhook",
            "rotate-secret",
            "ops",
            "00000000-0000-0000-0000-000000000001"
        ]),
        json!([
            "cache",
            "webhook",
            "create",
            "prod",
            "--name",
            "hook",
            "--url",
            "https://example.com"
        ]),
    ] {
        run_gradient_with_temp_home_stdin_and_args(&home, &build_mcp_stdin_json_rpc_frames_for_initialize_initialized_and_run_gradient_command_call(args), &["mcp", "--control"])
            .success()
            .stdout(predicate::str::contains("isn't available here"));
    }
}

#[tokio::test]
async fn mcp_run_gradient_command_refuses_task_action_create_that_prints_a_one_time_token_and_sends_no_request()
 {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        &build_mcp_stdin_json_rpc_frames_for_initialize_initialized_and_run_gradient_command_call(
            json!([
                "task",
                "--task",
                "ci",
                "action",
                "create",
                "--name",
                "hook",
                "--config",
                "{\"type\":\"send_web_request\",\"url\":\"https://example.com\"}",
                "--events",
                "evaluation.completed"
            ]),
        ),
        &["mcp", "--control"],
    )
    .success()
    .stdout(predicate::str::contains("isn't available here"));
}

#[tokio::test]
async fn mcp_allows_listing_secret_names() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/proj/secrets"))
        .respond_with(ok(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient_with_temp_home_stdin_and_args(
        &home,
        &build_mcp_stdin_json_rpc_frames_for_initialize_initialized_and_run_gradient_command_call(
            json!(["project", "secret", "list"]),
        ),
        &["mcp", "--control"],
    )
    .success()
    .stdout(predicate::str::contains("isn't available here").not());
}
