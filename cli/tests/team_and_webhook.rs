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
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOOK: &str = "00000000-0000-0000-0000-000000000001";

fn home_with(server: &MockServer) -> TempDir {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "Server = '{}'\nAuthToken = 'test-token'\nSelectedProject = 'web'\n",
            server.uri()
        ),
    )
    .unwrap();
    home
}

fn gradient(home: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_ACCOUNT")
        .args(args)
        .assert()
}

fn envelope(assert: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

async fn answer(
    server: &MockServer,
    verb: &str,
    endpoint: &str,
    body: Option<Value>,
    message: Value,
) {
    let mock = Mock::given(method(verb)).and(path(format!("/api/v1/{endpoint}")));
    let mock = match body {
        Some(body) => mock.and(body_json(body)),
        None => mock,
    };
    mock.respond_with(
        ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message})),
    )
    .expect(1)
    .mount(server)
    .await;
}

fn webhook() -> Value {
    json!({
        "id": HOOK,
        "scope": "team",
        "name": "notify",
        "url": "https://example.com/hook",
        "events": [],
        "active": true,
        "created_by": "00000000-0000-0000-0000-000000000002",
        "created_at": "2026-11-01T00:00:00",
        "updated_at": "2026-11-01T00:00:00"
    })
}

#[tokio::test]
async fn adding_a_member_invites_with_the_member_role_by_default() {
    let server = MockServer::start().await;
    answer(
        &server,
        "POST",
        "teams/ops/members",
        Some(json!({"user": "bob", "role": "member"})),
        json!("Invitation sent"),
    )
    .await;

    let home = home_with(&server);
    let assert = gradient(&home, &["--json", "team", "members", "add", "ops", "bob"]).success();
    assert_eq!(envelope(&assert)["message"]["message"], "Invitation sent");
}

#[tokio::test]
async fn editing_a_team_without_settings_is_a_usage_error() {
    let server = MockServer::start().await;
    let home = home_with(&server);
    gradient(&home, &["--json", "team", "edit", "ops"]).code(2);
}

#[tokio::test]
async fn deactivating_a_team_worker_patches_only_active() {
    let server = MockServer::start().await;
    answer(
        &server,
        "PATCH",
        "teams/ops/workers/w1",
        Some(json!({"active": false})),
        json!("updated"),
    )
    .await;

    let home = home_with(&server);
    gradient(
        &home,
        &[
            "--json", "team", "worker", "edit", "ops", "w1", "--active", "false",
        ],
    )
    .success();
}

#[tokio::test]
async fn webhook_list_targets_the_selected_project() {
    let server = MockServer::start().await;
    answer(
        &server,
        "GET",
        "projects/web/webhooks",
        None,
        json!([webhook()]),
    )
    .await;

    let home = home_with(&server);
    let assert = gradient(&home, &["--json", "webhook", "list"]).success();
    assert_eq!(envelope(&assert)["message"][0]["name"], "notify");
}

#[tokio::test]
async fn webhook_team_flag_targets_the_team_after_the_subcommand() {
    let server = MockServer::start().await;
    answer(
        &server,
        "PATCH",
        &format!("teams/ops/webhooks/{HOOK}"),
        Some(json!({"events": []})),
        webhook(),
    )
    .await;

    let home = home_with(&server);
    gradient(
        &home,
        &[
            "--json",
            "webhook",
            "edit",
            HOOK,
            "--all-events",
            "--team",
            "ops",
        ],
    )
    .success();
}

#[tokio::test]
async fn webhook_create_sends_every_event_pattern() {
    let server = MockServer::start().await;
    answer(
        &server,
        "POST",
        "projects/api/webhooks",
        Some(json!({
            "name": "notify",
            "url": "https://example.com/hook",
            "events": ["build.failed", "evaluation.*"]
        })),
        json!({"webhook": webhook(), "secret": "whsec"}),
    )
    .await;

    let home = home_with(&server);
    let assert = gradient(
        &home,
        &[
            "--json",
            "--project",
            "api",
            "webhook",
            "create",
            "--name",
            "notify",
            "--url",
            "https://example.com/hook",
            "--event",
            "build.failed",
            "--event",
            "evaluation.*",
        ],
    )
    .success();
    assert_eq!(envelope(&assert)["message"]["secret"], "whsec");
}
