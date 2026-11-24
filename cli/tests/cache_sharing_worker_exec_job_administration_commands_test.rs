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
            serde_json::json!({"user": "bob", "role": "View", "add_as_member_immediately": false}),
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

fn write_secret_text_to_new_file_in_test_home_and_return_path(
    home: &TempDir,
    file_name: &str,
    secret_text: &str,
) -> String {
    let path = home.path().join(file_name);
    fs::write(&path, secret_text).unwrap();
    path.to_str().unwrap().to_owned()
}

fn run_gradient_binary_with_human_readable_output_in_temporary_home(
    home: &TempDir,
    args: &[&str],
) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_TOKEN")
        .args(args)
        .assert()
}

#[tokio::test]
async fn test_gradient_cache_upstream_add_with_basic_auth_user_and_password_file_sends_basic_credential_and_trims_trailing_line_break()
 {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/caches/prod/upstream-caches"))
        .and(body_json(serde_json::json!({
            "type": "http",
            "display_name": "Company",
            "url": "https://cache.example.com",
            "public_key": "cache.example.com-1:key",
            "http1_only": false,
            "credential": {"scheme": "basic", "user": "alice", "password": "s3cret"}
        })))
        .respond_with(ok(serde_json::json!(
            "00000000-0000-0000-0000-00000000000c"
        )))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let password_file =
        write_secret_text_to_new_file_in_test_home_and_return_path(&home, "password", "s3cret\n");
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "add",
            "prod",
            "--display-name",
            "Company",
            "--url",
            "https://cache.example.com",
            "--public-key",
            "cache.example.com-1:key",
            "--basic-auth-user",
            "alice",
            "--basic-auth-password-file",
            &password_file,
        ],
    )
    .success();
}

#[tokio::test]
async fn cache_upstream_add_with_bearer_token_file_sends_http_upstream_request_with_bearer_credential()
 {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/caches/prod/upstream-caches"))
        .and(body_json(serde_json::json!({
            "type": "http",
            "display_name": "Company",
            "url": "https://cache.example.com",
            "public_key": "cache.example.com-1:key",
            "http1_only": false,
            "credential": {"scheme": "bearer", "token": "tok"}
        })))
        .respond_with(ok(serde_json::json!(
            "00000000-0000-0000-0000-00000000000c"
        )))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let token_file =
        write_secret_text_to_new_file_in_test_home_and_return_path(&home, "token", "tok");
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "add",
            "prod",
            "--display-name",
            "Company",
            "--url",
            "https://cache.example.com",
            "--public-key",
            "cache.example.com-1:key",
            "--bearer-token-file",
            &token_file,
        ],
    )
    .success();
}

#[tokio::test]
async fn test_cache_upstream_edit_bearer_token_file_sends_patch_with_new_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/prod/upstream-caches/u1"))
        .and(body_json(serde_json::json!({
            "credential": {"scheme": "bearer", "token": "new-token"}
        })))
        .respond_with(ok(serde_json::json!("updated")))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let token_file =
        write_secret_text_to_new_file_in_test_home_and_return_path(&home, "token", "new-token\r\n");
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "edit",
            "prod",
            "u1",
            "--bearer-token-file",
            &token_file,
        ],
    )
    .success();
}

#[tokio::test]
async fn cache_upstream_edit_remove_credential_sends_patch_with_none_credential_scheme() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/prod/upstream-caches/u1"))
        .and(body_json(serde_json::json!({
            "credential": {"scheme": "none"}
        })))
        .respond_with(ok(serde_json::json!("updated")))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "edit",
            "prod",
            "u1",
            "--remove-credential",
        ],
    )
    .success();
}

#[tokio::test]
async fn cache_upstream_edit_without_credential_flags_sends_patch_without_credential_field() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/prod/upstream-caches/u1"))
        .and(body_json(serde_json::json!({"display_name": "Renamed"})))
        .respond_with(ok(serde_json::json!("updated")))
        .expect(1)
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "edit",
            "prod",
            "u1",
            "--display-name",
            "Renamed",
        ],
    )
    .success();
}

#[tokio::test]
async fn rejects_conflicting_credential_flag_combinations_without_sending_request() {
    let server = MockServer::start().await;
    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let secret_file =
        write_secret_text_to_new_file_in_test_home_and_return_path(&home, "secret", "value");
    let http_add = [
        "cache",
        "upstream",
        "add",
        "prod",
        "--display-name",
        "Company",
        "--url",
        "https://cache.example.com",
        "--public-key",
        "cache.example.com-1:key",
    ];
    let edit = ["cache", "upstream", "edit", "prod", "u1"];
    let conflicting_flag_combinations: [Vec<&str>; 8] = [
        [&http_add[..], &["--basic-auth-user", "alice"]].concat(),
        [&http_add[..], &["--basic-auth-password-file", &secret_file]].concat(),
        [
            &http_add[..],
            &[
                "--basic-auth-user",
                "alice",
                "--basic-auth-password-file",
                &secret_file,
                "--bearer-token-file",
                &secret_file,
            ],
        ]
        .concat(),
        vec![
            "cache",
            "upstream",
            "add",
            "prod",
            "--internal-cache-name",
            "base",
            "--bearer-token-file",
            &secret_file,
        ],
        vec![
            "cache",
            "upstream",
            "add",
            "prod",
            "--display-name",
            "Other",
            "--url",
            "https://other.example",
            "--remote-cache-name",
            "theirs",
            "--bearer-token-file",
            &secret_file,
        ],
        [
            &edit[..],
            &["--remove-credential", "--bearer-token-file", &secret_file],
        ]
        .concat(),
        [
            &edit[..],
            &["--remove-credential", "--basic-auth-user", "alice"],
        ]
        .concat(),
        [&edit[..], &["--basic-auth-user", "alice"]].concat(),
    ];
    for args in conflicting_flag_combinations {
        run_gradient_binary_with_json_in_home(&home, &args)
            .failure()
            .code(2);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn rejects_empty_bearer_token_file_and_sends_no_request() {
    let server = MockServer::start().await;
    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let token_file =
        write_secret_text_to_new_file_in_test_home_and_return_path(&home, "token", "\n");
    let assert = run_gradient_binary_with_json_in_home(
        &home,
        &[
            "cache",
            "upstream",
            "edit",
            "prod",
            "u1",
            "--bearer-token-file",
            &token_file,
        ],
    )
    .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(&token_file), "{stdout}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cache_upstream_list_prints_credential_kind_and_marks_rows_with_user_password_or_query() {
    let server = MockServer::start().await;
    let upstream_cache_item = |id: &str, scheme: Option<&str>, stored_url_is_unsafe: bool| {
        serde_json::json!({
            "id": id,
            "display_name": "Company",
            "mode": "ReadOnly",
            "kind": "http",
            "url": "https://cache.example.com",
            "http1_only": false,
            "active": !stored_url_is_unsafe,
            "stored_url_has_user_password_or_query": stored_url_is_unsafe,
            "credential_scheme": scheme
        })
    };
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/prod/upstream-caches"))
        .respond_with(ok(serde_json::json!([
            upstream_cache_item("00000000-0000-0000-0000-00000000000a", Some("basic"), false),
            upstream_cache_item(
                "00000000-0000-0000-0000-00000000000b",
                Some("bearer"),
                false
            ),
            upstream_cache_item("00000000-0000-0000-0000-00000000000c", None, true),
        ])))
        .mount(&server)
        .await;

    let home = temp_home_with_gradient_config_token_and_project_for_mock_server(&server);
    let assert = run_gradient_binary_with_human_readable_output_in_temporary_home(
        &home,
        &["cache", "upstream", "list", "prod"],
    )
    .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[0].contains("credential: basic"), "{stdout}");
    assert!(lines[1].contains("credential: bearer"), "{stdout}");
    assert!(lines[2].contains("credential: none"), "{stdout}");
    assert!(
        lines[2].contains("[URL with a username, password or query]"),
        "{stdout}"
    );
    assert!(!lines[0].contains("[URL with"), "{stdout}");

    let json_assert =
        run_gradient_binary_with_json_in_home(&home, &["cache", "upstream", "list", "prod"])
            .success();
    let json_stdout = String::from_utf8(json_assert.get_output().stdout.clone()).unwrap();
    assert!(
        json_stdout.contains("\"credential_scheme\":\"bearer\""),
        "{json_stdout}"
    );
    assert!(
        json_stdout.contains("\"stored_url_has_user_password_or_query\":true"),
        "{json_stdout}"
    );
}

#[tokio::test]
async fn test_gradient_cache_webhook_create_posts_webhook_events_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/prod/webhooks"))
        .and(body_json(serde_json::json!({
            "name": "hook",
            "url": "https://example.com/hook",
            "events": ["cache.*", "nar.pushed"],
            "active": true
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
                "created_at": "2026-11-09T00:00:00",
                "updated_at": "2026-11-09T00:00:00"
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
