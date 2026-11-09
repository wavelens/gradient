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
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INBOUND: &str = "00000000-0000-0000-0000-000000000001";
const OUTBOUND: &str = "00000000-0000-0000-0000-000000000002";
const ACTION: &str = "00000000-0000-0000-0000-000000000003";
const OVERRIDE: &str = "00000000-0000-0000-0000-000000000007";
const TRIGGER: &str = "00000000-0000-0000-0000-000000000008";
const TASK_ID: &str = "00000000-0000-0000-0000-000000000006";

fn ok(message: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({"error": false, "message": message}))
}

fn home_with(server: &MockServer) -> TempDir {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();
    home
}

fn run_gradient_json_with_temp_home(home: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_ACCOUNT")
        .arg("--json")
        .args(["--task", "proj/nightly"])
        .args(args)
        .assert()
}

#[tokio::test]
async fn test_cli_gradient_search_passes_query_and_limit_and_prints_json_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("q", "nightly"))
        .and(query_param("limit", "2"))
        .respond_with(ok(serde_json::json!([{
            "kind": "task", "label": "Nightly", "sublabel": "proj/nightly",
            "route": "/projects/proj/tasks/nightly", "starred": false
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = run_gradient_json_with_temp_home(&home, &["search", "nightly", "--limit", "2"])
        .success()
        .get_output()
        .stdout
        .clone();
    let envelope: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["message"][0]["kind"], "task");
}

#[tokio::test]
async fn test_task_repository_edit_push_branches_keeps_other_current_settings_in_full_put_body() {
    let server = MockServer::start().await;
    let repository = serde_json::json!({
        "repository": "https://git.example.com/org/repo.git", "uses_ssh": false,
        "build_on_push": {
            "trigger_id": TRIGGER, "integration_id": INBOUND, "active": true,
            "branches": ["dev"], "tags": ["v*"]
        },
        "report_status": {"action_id": ACTION, "integration_id": OUTBOUND, "active": true},
        "integrations": [{
            "id": INBOUND, "name": "forgejo-hooks", "display_name": "Forgejo hooks",
            "kind": "inbound", "git_host_type": "forgejo", "serves_repository": true
        }],
        "can_edit": true, "can_manage_integrations": true
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/repository"))
        .respond_with(ok(repository.clone()))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/tasks/proj/nightly/repository"))
        .and(body_json(serde_json::json!({
            "inbound_integration": INBOUND,
            "build_on_push": true,
            "push_branches": ["main"],
            "push_tags": ["v*"],
            "build_on_pull_request": false,
            "require_approval": true,
            "outbound_integration": OUTBOUND,
            "report_status": true
        })))
        .respond_with(ok(repository))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(
        &home,
        &[
            "task",
            "repository",
            "edit",
            "--inbound-integration",
            "forgejo-hooks",
            "--push-branches",
            "main",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_action_delete_by_name_looks_up_and_deletes_by_uuid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/actions"))
        .respond_with(ok(serde_json::json!([{
            "id": ACTION, "name": "deploy-hook", "action_type": "send_web_request",
            "config": {"url": "https://example.com/hook"}, "events": [], "active": true,
            "created_by": TASK_ID, "created_at": "2026-01-01T00:00:00",
            "updated_at": "2026-01-01T00:00:00"
        }])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/tasks/proj/nightly/actions/{ACTION}")))
        .respond_with(ok(serde_json::json!({"deleted": true})))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(&home, &["task", "action", "delete", "deploy-hook"]).success();
}

#[tokio::test]
async fn test_flake_input_edit_clear_url_sends_null_url() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/v1/tasks/proj/nightly/flake-inputs/{OVERRIDE}"
        )))
        .and(body_json(serde_json::json!({"url": null})))
        .respond_with(ok(serde_json::json!({
            "id": OVERRIDE, "task": TASK_ID, "input_name": "nixpkgs", "url": null,
            "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(
        &home,
        &["task", "flake-input", "edit", OVERRIDE, "--clear-url"],
    )
    .success();
}

#[tokio::test]
async fn test_task_trigger_default_and_task_transfer_call_matching_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/tasks/proj/nightly/triggers/{TRIGGER}/default"
        )))
        .respond_with(ok(serde_json::json!({
            "id": TRIGGER, "task": TASK_ID, "type": "time", "config": {"cron": "0 3 * * *"},
            "active": true, "is_default": true,
            "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/transfer"))
        .and(body_json(serde_json::json!({"project": "other"})))
        .respond_with(ok(serde_json::json!("moved")))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(&home, &["task", "trigger", "default", TRIGGER]).success();
    run_gradient_json_with_temp_home(&home, &["task", "transfer", "other"]).success();
}

#[tokio::test]
async fn test_action_create_with_invalid_config_json_exits_usage_error_before_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(serde_json::json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(
        &home,
        &[
            "task", "action", "create", "--name", "x", "--config", "{bad",
        ],
    )
    .code(2);
}
