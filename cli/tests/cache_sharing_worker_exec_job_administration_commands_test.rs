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
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn temp_home_with_gradient_config_token_and_project_for_mock_server(
    server: &MockServer,
) -> TempDir {
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

fn run_gradient_binary_with_json_in_home(
    home: &TempDir,
    args: &[&str],
) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_TOKEN")
        .arg("--json")
        .args(args)
        .assert()
}

fn ok(message: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({"error": false, "message": message}))
}

#[tokio::test]
async fn post_user_and_role_to_cache_members() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/prod/members"))
        .and(body_json(
            serde_json::json!({"user": "bob", "role": "View"}),
        ))
        .respond_with(ok(serde_json::json!("invited")))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(&home, &["cache", "user", "add", "prod", "bob", "View"])
        .success();
}

#[tokio::test]
async fn test_gradient_cache_upstream_add_with_internal_cache_name_puts_internal_upstream_request()
{
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/caches/prod/upstream-caches"))
        .and(body_json(
            serde_json::json!({"type": "internal", "cache_name": "base"}),
        ))
        .respond_with(ok(serde_json::json!(
            "00000000-0000-0000-0000-00000000000c"
        )))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let assert = run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "add",
            "prod",
            "--internal-cache-name",
            "base",
        ],
    )
    .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("00000000-0000-0000-0000-00000000000c"),
        "{stdout}"
    );
}

#[tokio::test]
async fn gradient_cache_upstream_add_rejects_both_internal_and_remote_cache_names() {
    let server = MockServer::start().await;
    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "add",
            "prod",
            "--internal-cache-name",
            "base",
            "--url",
            "https://other.example",
            "--remote-cache-name",
            "theirs",
        ],
    )
    .failure();
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn test_gradient_cache_webhook_create_posts_webhook_events_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/prod/webhooks"))
        .and(body_json(serde_json::json!({
            "name": "hook",
            "url": "https://example.com/hook",
            "events": ["cache.*", "nar.pushed"]
        })))
        .respond_with(ok(serde_json::json!({
            "secret": "whsec",
            "webhook": {
                "id": "00000000-0000-0000-0000-00000000000d",
                "scope": "cache",
                "name": "hook",
                "url": "https://example.com/hook",
                "events": ["cache.*", "nar.pushed"],
                "active": true,
                "created_by": "00000000-0000-0000-0000-00000000000e",
                "created_at": "2026-11-09T00:00:00Z",
                "updated_at": "2026-11-09T00:00:00Z"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let assert = run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "webhook",
            "create",
            "prod",
            "--name",
            "hook",
            "--url",
            "https://example.com/hook",
            "--event",
            "cache.*",
            "--event",
            "nar.pushed",
        ],
    )
    .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("whsec"), "{stdout}");
}

#[tokio::test]
async fn gradient_worker_check_w_posts_to_selected_project_worker_test_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/proj/workers/w1/test"))
        .respond_with(ok(serde_json::json!({
            "ok": true,
            "connected": true,
            "authorized_for_project": true,
            "message": "ready"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(&home, &["worker", "check", "w1"]).success();
}

#[tokio::test]
async fn gradient_exec_job_keep_vm_running_off_sends_delete_request() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/exec/job1/keep-vm-running"))
        .respond_with(ok(serde_json::json!("released")))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(&home, &["exec-job", "keep-vm-running", "job1", "off"])
        .success();
}

#[tokio::test]
async fn gradient_json_exec_job_console_fails_with_usage_error_before_server_contact() {
    let server = MockServer::start().await;
    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(&home, &["exec-job", "console", "job1"]).code(2);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn post_gradient_administration_drain_on_enabled_true() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/draining"))
        .and(body_json(serde_json::json!({"enabled": true})))
        .respond_with(ok(serde_json::json!({"draining": true})))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let assert =
        run_gradient_binary_with_json_in_home(&home, &["administration", "drain", "on"]).success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"draining\":true"), "{stdout}");
}
