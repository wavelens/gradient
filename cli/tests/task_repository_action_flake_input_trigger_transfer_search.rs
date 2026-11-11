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

const REPOSITORY: &str = "https://git.example.com/org/repo.git";

async fn mount_push_and_pull_request_suggestions(
    server: &MockServer,
    suggestions: serde_json::Value,
) {
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/proj/trigger-suggestions"))
        .and(query_param("repository", REPOSITORY))
        .respond_with(ok(suggestions))
        .mount(server)
        .await;
}

async fn mount_task_create(server: &MockServer, expected_calls: u64) {
    Mock::given(method("PUT"))
        .and(path("/api/v1/tasks/proj"))
        .respond_with(ok(serde_json::json!(TASK_ID)))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn sent_task_create_body(server: &MockServer) -> serde_json::Value {
    let requests = server.received_requests().await.unwrap();
    let create = requests
        .iter()
        .find(|request| request.method.as_str() == "PUT")
        .unwrap();
    serde_json::from_slice(&create.body).unwrap()
}

fn create_task_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "--project",
        "proj",
        "task",
        "create",
        "-n",
        "web",
        "-d",
        "Web",
        "-c",
        "",
        "-r",
        REPOSITORY,
        "-w",
        "packages.*",
    ];
    args.extend_from_slice(extra);
    args
}

#[tokio::test]
async fn test_task_create_sends_branch_cron_and_suggested_push_trigger_in_one_request() {
    let server = MockServer::start().await;
    mount_push_and_pull_request_suggestions(
        &server,
        serde_json::json!([
            {"type": "reporter_push", "include_flake_inputs": true},
            {"type": "reporter_pull_request"}
        ]),
    )
    .await;
    mount_task_create(&server, 1).await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(
        &home,
        &create_task_args(&[
            "--branch",
            "release",
            "--trigger",
            "cron=0 3 * * *",
            "--trigger",
            "push",
        ]),
    )
    .success();

    let triggers = sent_task_create_body(&server).await["triggers"].clone();
    assert_eq!(triggers.as_array().unwrap().len(), 2, "{triggers}");
    assert_eq!(triggers[0]["type"], "time");
    assert_eq!(triggers[0]["cron"], "0 3 * * *");
    assert_eq!(triggers[0]["branch"], "release");
    assert_eq!(triggers[1]["type"], "reporter_push");
    assert_eq!(triggers[1]["include_flake_inputs"], true);
    assert_eq!(triggers[1]["branches"], serde_json::json!(["release"]));
}

#[tokio::test]
async fn test_task_create_leaves_triggers_to_the_server_unless_asked_and_can_skip_the_default() {
    let server = MockServer::start().await;
    mount_task_create(&server, 1).await;
    let home = home_with(&server);
    run_gradient_json_with_temp_home(&home, &create_task_args(&[])).success();
    assert!(
        sent_task_create_body(&server)
            .await
            .get("triggers")
            .is_none_or(serde_json::Value::is_null)
    );

    let server = MockServer::start().await;
    mount_task_create(&server, 1).await;
    let home = home_with(&server);
    run_gradient_json_with_temp_home(&home, &create_task_args(&["--no-default-trigger"])).success();
    assert_eq!(
        sent_task_create_body(&server).await["triggers"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn test_task_create_with_push_trigger_and_no_webhook_receiver_creates_nothing() {
    let server = MockServer::start().await;
    mount_push_and_pull_request_suggestions(&server, serde_json::json!([])).await;
    mount_task_create(&server, 0).await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(&home, &create_task_args(&["--trigger", "pr"])).failure();
}

#[tokio::test]
async fn test_task_duplicate_posts_the_new_names_and_branch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/duplicate"))
        .and(body_json(serde_json::json!({
            "name": "nightly-release", "display_name": "Nightly release", "branch": "release"
        })))
        .respond_with(ok(serde_json::json!(TASK_ID)))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    run_gradient_json_with_temp_home(
        &home,
        &[
            "task",
            "duplicate",
            "--name",
            "nightly-release",
            "--display-name",
            "Nightly release",
            "--branch",
            "release",
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
