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

fn run_gradient(home: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("TMPDIR", home.path())
        .env_remove("GRADIENT_TOKEN")
        .env_remove("GRADIENT_SERVER")
        .env_remove("GRADIENT_ACCOUNT")
        .args(args)
        .assert()
}

fn ok(message: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message}))
}

fn team(name: &str) -> Value {
    json!({"name": name, "display_name": name, "role": "member"})
}

async fn mount_teams_and_expect_project_create(server: &MockServer, teams: Value, body: Value) {
    Mock::given(method("GET"))
        .and(path("/api/v1/teams"))
        .respond_with(ok(teams))
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects"))
        .and(body_json(body))
        .respond_with(ok(
            json!({"id": "created-id", "name": "p", "url": "https://gradient.example/project/p"}),
        ))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn project_create_without_team_sends_users_only_team_as_owner_team() {
    let server = MockServer::start().await;
    mount_teams_and_expect_project_create(
        &server,
        json!([team("ops")]),
        json!({"name": "p", "display_name": "P", "description": "d", "owner_team": "ops"}),
    )
    .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient(
        &home,
        &[
            "--json",
            "project",
            "create",
            "--name",
            "p",
            "--display-name",
            "P",
            "--description",
            "d",
        ],
    )
    .success()
    .stdout(predicate::str::contains("\"id\":\"created-id\""));
}

#[tokio::test]
async fn project_create_sends_no_owner_team_when_user_is_member_of_two_teams() {
    let server = MockServer::start().await;
    mount_teams_and_expect_project_create(
        &server,
        json!([team("ops"), team("dev")]),
        json!({"name": "p", "display_name": "P", "description": "d"}),
    )
    .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient(
        &home,
        &[
            "--json",
            "project",
            "create",
            "--name",
            "p",
            "--display-name",
            "P",
            "--description",
            "d",
        ],
    )
    .success();
}

#[tokio::test]
async fn project_access_output_lists_each_person_with_role_reasons_and_marks_overridden_team_grants()
 {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/proj/access"))
        .respond_with(ok(json!([{
            "user": "alice",
            "user_display_name": "Alice",
            "role": "View",
            "permissions": ["View"],
            "reasons": [
                {"kind": "direct", "team": null, "role": "View", "applies": true, "managed": false},
                {
                    "kind": "owner",
                    "team": {"name": "ops", "display_name": "Ops"},
                    "role": "Admin",
                    "applies": false,
                    "managed": false
                }
            ]
        }])))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient(&home, &["project", "access"])
        .success()
        .stdout(predicate::str::contains(
            "alice: View (direct, owner team ops: Admin (overridden))",
        ));
}

#[tokio::test]
async fn test_user_access_for_another_user_with_no_access_prints_no_access() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/carol/access"))
        .respond_with(ok(
            json!({"superuser": false, "projects": [], "caches": [], "teams": []}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let home = temp_home_with_wiremock_gradient_config_token_and_project(&server);

    run_gradient(&home, &["user", "access", "carol"])
        .success()
        .stdout(predicate::str::contains("No access."));
}
