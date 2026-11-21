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
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT_CONNECTION: &str = "00000000-0000-0000-0000-000000006200";
const TEAM_CONNECTION: &str = "00000000-0000-0000-0000-000000006201";
const OTHER_CONNECTION: &str = "00000000-0000-0000-0000-000000006202";
const USER: &str = "00000000-0000-0000-0000-000000006203";
const DELIVERY: &str = "00000000-0000-0000-0000-000000006204";
const ATTEMPT: &str = "00000000-0000-0000-0000-000000006205";
const REPOSITORY_GRANT_UUID: &str = "71da92de-bc06-4bb7-9d98-5bcfb9a5a4da";
const CONNECTIONS: &str = "/api/v1/projects/web/git-host-connections";
const REPOSITORY: &str = "/api/v1/tasks/web/nightly/repository";

fn ok(message: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message}))
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

fn gradient(home: &TempDir, json: bool, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = Command::cargo_bin("gradient").unwrap();
    command
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_ACCOUNT");
    if json {
        command.arg("--json");
    }
    command
        .args(["--project", "web", "--task", "web/nightly"])
        .args(args)
        .assert()
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.success().get_output().stdout.clone()).unwrap()
}

fn summary(id: &str, name: &str, owner: Value, reports_status: bool) -> Value {
    json!({
        "id": id, "owner": owner, "name": name, "display_name": name.to_uppercase(),
        "git_host_type": "forgejo", "base_url": "https://git.example.com",
        "receives_events": true, "reports_status": reports_status, "can_manage": true,
        "webhook_url": "https://gradient.example/api/v1/hooks/forgejo"
    })
}

fn project_owner() -> Value {
    json!({"type": "project", "name": "web"})
}

fn team_owner() -> Value {
    json!({"type": "team", "name": "ops"})
}

fn connection(id: &str, generated_secret: Option<&str>) -> Value {
    json!({
        "id": id, "owner": project_owner(), "name": "forgejo", "display_name": "Forgejo",
        "git_host_type": "forgejo", "base_url": "https://git.example.com", "has_secret": true,
        "has_access_token": true, "access_token_refreshes_automatically": false,
        "allowed_ips": [], "receives_events": true,
        "reports_status": true, "created_by": USER, "created_at": "2026-11-11T00:00:00Z",
        "managed": false, "webhook_url": "https://gradient.example/api/v1/hooks/forgejo",
        "generated_secret": generated_secret
    })
}

async fn mount_project_connections_with_same_name_in_team(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(CONNECTIONS))
        .respond_with(ok(json!([
            summary(TEAM_CONNECTION, "forgejo", team_owner(), true),
            summary(PROJECT_CONNECTION, "forgejo", project_owner(), true),
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn test_git_host_connection_list_defaults_to_the_selected_project() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        true,
        &["project", "git-host-connection", "list"],
    ));
    let envelope: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(envelope["message"][1]["id"], PROJECT_CONNECTION);

    let output = stdout_of(gradient(
        &home,
        false,
        &["project", "git-host-connection", "list"],
    ));
    assert!(output.contains("team/ops"), "{output}");
    assert!(output.contains("project/web"), "{output}");
}

#[tokio::test]
async fn server_names_and_error_messages_are_printed_to_terminal_with_control_and_escape_sequences_removed()
 {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CONNECTIONS))
        .respond_with(ok(json!([summary(
            PROJECT_CONNECTION,
            "evil\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[2Kname\u{202e}txt.exe\rcovered",
            project_owner(),
            true
        )])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(REPOSITORY))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": true,
            "message": "boom\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[2J\u{202e}"
        })))
        .mount(&server)
        .await;
    let home = home_with(&server);

    let table = stdout_of(gradient(
        &home,
        false,
        &["project", "git-host-connection", "list"],
    ));
    assert!(table.starts_with("COVERED"), "{table:?}");
    for leaked in ["\u{1b}", "\u{7}", "\u{202e}", "\r", "52;c"] {
        assert!(!table.contains(leaked), "{table:?}");
    }

    let failure = gradient(&home, false, &["task", "repository", "show"]).failure();
    let error_text = String::from_utf8(failure.get_output().stderr.clone()).unwrap();
    assert!(error_text.contains("boom"), "{error_text:?}");
    for leaked in ["\u{1b}", "\u{7}", "\u{202e}", "52;c"] {
        assert!(!error_text.contains(leaked), "{error_text:?}");
    }
}

#[tokio::test]
async fn test_git_host_connection_create_for_a_team_sends_flags_and_prints_secret_once() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams/ops/git-host-connections"))
        .and(body_json(json!({
            "name": "forgejo", "git_host_type": "forgejo", "display_name": "Forgejo",
            "base_url": "https://git.example.com", "access_token": "token",
            "allowed_ips": ["10.0.0.0/8", "192.168.1.1"], "receives_events": true,
            "reports_status": false
        })))
        .respond_with(ok(connection(TEAM_CONNECTION, Some("generated-secret"))))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &[
            "project",
            "git-host-connection",
            "create",
            "--owner",
            "team/ops",
            "--name",
            "forgejo",
            "--host-type",
            "forgejo",
            "--display-name",
            "Forgejo",
            "--base-url",
            "https://git.example.com",
            "--access-token",
            "token",
            "--allowed-ips",
            "10.0.0.0/8,192.168.1.1",
            "--receives-events",
            "true",
            "--reports-status",
            "false",
        ],
    ));
    assert_eq!(output.matches("generated-secret").count(), 1, "{output}");
    assert!(
        output.contains("https://gradient.example/api/v1/hooks/forgejo"),
        "{output}"
    );
}

#[tokio::test]
async fn test_git_host_connection_create_rejects_unknown_host_type_before_request() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "create",
            "--name",
            "x",
            "--host-type",
            "bitbucket",
        ],
    )
    .code(2);
}

#[tokio::test]
async fn test_git_host_connection_show_by_name_picks_the_connection_of_the_owner() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CONNECTIONS}/{PROJECT_CONNECTION}")))
        .respond_with(ok(connection(PROJECT_CONNECTION, None)))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &["project", "git-host-connection", "show", "forgejo"],
    )
    .success();
}

#[tokio::test]
async fn test_git_host_connection_edit_sends_only_given_flags_and_clears_with_empty_values() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CONNECTIONS}/{PROJECT_CONNECTION}")))
        .and(body_json(json!({
            "name": "forgejo-main", "access_token": "", "allowed_ips": [],
            "reports_status": false
        })))
        .respond_with(ok(connection(PROJECT_CONNECTION, None)))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "edit",
            "forgejo",
            "--name",
            "forgejo-main",
            "--access-token",
            "",
            "--allowed-ips",
            "",
            "--reports-status",
            "false",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_git_host_connection_edit_prints_generated_webhook_secret_once_like_create() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CONNECTIONS}/{PROJECT_CONNECTION}")))
        .and(body_json(json!({"base_url": "https://other.example.com"})))
        .respond_with(ok(connection(PROJECT_CONNECTION, Some("generated-secret"))))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &[
            "project",
            "git-host-connection",
            "edit",
            "forgejo",
            "--base-url",
            "https://other.example.com",
        ],
    ));
    assert_eq!(output.matches("generated-secret").count(), 1, "{output}");
    assert!(
        output.contains("Webhook secret: generated-secret"),
        "{output}"
    );
}

#[tokio::test]
async fn test_git_host_connection_delete_and_deliveries_by_uuid_skip_the_name_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/git-host-connections"))
        .respond_with(ok(json!([])))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v1/admin/git-host-connections/{OTHER_CONNECTION}"
        )))
        .respond_with(ok(json!(true)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/admin/git-host-connections/{OTHER_CONNECTION}/deliveries"
        )))
        .respond_with(ok(json!([{
            "id": DELIVERY, "event": "push", "status_code": 202, "result": "queued",
            "repository": "org/repo", "tasks_scanned": 1, "queued": 1,
            "created_at": "2026-11-11T00:00:00Z"
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &[
            "project",
            "git-host-connection",
            "deliveries",
            OTHER_CONNECTION,
            "--owner",
            "instance",
        ],
    ));
    assert!(output.contains("push 202 queued org/repo"), "{output}");
    gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "delete",
            OTHER_CONNECTION,
            "--owner",
            "instance",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_git_host_connection_rejects_invalid_owner_and_unknown_name() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &["project", "git-host-connection", "list", "--owner", "org/x"],
    )
    .code(2);
    gradient(
        &home,
        true,
        &["project", "git-host-connection", "show", "gitlab"],
    )
    .code(2);
}

fn one_repository_grant_response_json() -> Value {
    json!({
        "id": REPOSITORY_GRANT_UUID, "project": USER, "project_name": "web",
        "repository_owner": "org", "repository_name": "repo", "source": "api",
        "created_by": USER, "created_at": "2026-11-11T00:00:00Z"
    })
}

#[tokio::test]
async fn test_repository_grant_create_list_and_delete_by_uuid_for_a_team_connection_sends_project_and_repository_flags_and_prints_grant()
 {
    let server = MockServer::start().await;
    let grants =
        format!("/api/v1/teams/ops/git-host-connections/{TEAM_CONNECTION}/repository-grants");
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/ops/git-host-connections"))
        .respond_with(ok(json!([summary(
            TEAM_CONNECTION,
            "forgejo",
            team_owner(),
            true
        )])))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(grants.clone()))
        .and(body_json(json!({
            "project": "web", "repository_owner": "org", "repository_name": "repo"
        })))
        .respond_with(ok(one_repository_grant_response_json()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(grants.clone()))
        .respond_with(ok(json!([one_repository_grant_response_json()])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{grants}/{REPOSITORY_GRANT_UUID}")))
        .respond_with(ok(json!(true)))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let forgejo_repository_grant_arguments_prefix = [
        "project",
        "git-host-connection",
        "repository-grants",
        "forgejo",
        "--owner",
        "team/ops",
    ];
    let created = stdout_of(gradient(
        &home,
        false,
        &[
            &forgejo_repository_grant_arguments_prefix[..],
            &[
                "create",
                "--repository-owner",
                "org",
                "--repository-name",
                "repo",
            ],
        ]
        .concat(),
    ));
    assert!(created.contains("org/repo web api"), "{created}");
    let listed = stdout_of(gradient(
        &home,
        false,
        &[&forgejo_repository_grant_arguments_prefix[..], &["list"]].concat(),
    ));
    assert!(
        listed.contains(&format!("org/repo web api: {REPOSITORY_GRANT_UUID}")),
        "{listed}"
    );
    gradient(
        &home,
        true,
        &[
            &forgejo_repository_grant_arguments_prefix[..],
            &["delete", REPOSITORY_GRANT_UUID],
        ]
        .concat(),
    )
    .success();
}

#[tokio::test]
async fn test_repository_grant_rejects_invalid_project_owner_with_exit_code_2() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CONNECTIONS))
        .respond_with(ok(json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "repository-grants",
            "forgejo",
            "list",
        ],
    )
    .code(2);
}

#[tokio::test]
async fn repository_grant_create_conflict_prints_api_message_and_fails() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v1/admin/git-host-connections/{OTHER_CONNECTION}/repository-grants"
        )))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": true,
            "message": "Repository grant already present."
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let failure = gradient(
        &home,
        false,
        &[
            "project",
            "git-host-connection",
            "repository-grants",
            OTHER_CONNECTION,
            "--owner",
            "instance",
            "create",
            "--repository-owner",
            "org",
            "--all-repositories",
        ],
    )
    .failure();
    let error_text = String::from_utf8(failure.get_output().stderr.clone()).unwrap();
    assert!(
        error_text.contains("Repository grant already present."),
        "{error_text:?}"
    );
}

fn repository_grant_create_command_arguments() -> Vec<&'static str> {
    vec![
        "project",
        "git-host-connection",
        "repository-grants",
        OTHER_CONNECTION,
        "--owner",
        "instance",
        "create",
        "--repository-owner",
        "org",
    ]
}

async fn mock_put_instance_connection_repository_grant_with_expected_body_and_call_count(
    server: &MockServer,
    body: Value,
    expected_calls: u64,
) {
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v1/admin/git-host-connections/{OTHER_CONNECTION}/repository-grants"
        )))
        .and(body_json(body))
        .respond_with(ok(one_repository_grant_response_json()))
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn repository_grants_create_with_only_repository_owner_exits_with_usage_code_two_and_sends_no_request()
 {
    let server = MockServer::start().await;
    mock_put_instance_connection_repository_grant_with_expected_body_and_call_count(
        &server,
        json!({ "project": "web", "repository_owner": "org" }),
        0,
    )
    .await;

    let home = home_with(&server);
    gradient(&home, true, &repository_grant_create_command_arguments()).code(2);
}

#[tokio::test]
async fn repository_grants_create_with_repository_name_and_all_repositories_exits_usage_without_request()
 {
    let server = MockServer::start().await;
    mock_put_instance_connection_repository_grant_with_expected_body_and_call_count(
        &server,
        json!({ "project": "web", "repository_owner": "org", "repository_name": "repo" }),
        0,
    )
    .await;

    let home = home_with(&server);
    let mut arguments = repository_grant_create_command_arguments();
    arguments.extend(["--repository-name", "repo", "--all-repositories"]);
    gradient(&home, true, &arguments).code(2);
}

#[tokio::test]
async fn repository_grants_create_with_all_repositories_sends_body_without_repository_name() {
    let server = MockServer::start().await;
    mock_put_instance_connection_repository_grant_with_expected_body_and_call_count(
        &server,
        json!({ "project": "web", "repository_owner": "org" }),
        1,
    )
    .await;

    let home = home_with(&server);
    let mut arguments = repository_grant_create_command_arguments();
    arguments.push("--all-repositories");
    gradient(&home, true, &arguments).success();
}

fn task_repository(report_status: bool, current: Option<Value>) -> Value {
    json!({
        "repository": "https://git.example.com/org/repo.git", "uses_ssh": false,
        "git_host_connection": current,
        "report_status": report_status,
        "chain": {
            "repository": {"state": "ok", "detail": "git.example.com/org/repo"},
            "connection": {"state": "ok", "detail": "forgejo"},
            "incoming_events": {"state": "ok", "detail": "push"},
            "commit_status": {"state": "failing", "detail": "bad token", "action": {"type": "test_status_report"}}
        },
        "recent_deliveries": [{
            "id": DELIVERY, "event": "push", "status_code": 202, "result": "queued",
            "tasks_scanned": 1, "queued": 1, "created_at": "2026-11-11T00:00:00Z"
        }],
        "status_posts": [{
            "id": ATTEMPT, "event": "evaluation.completed", "success": false,
            "response_status": 401, "error_message": "bad token",
            "delivered_at": "2026-11-11T00:00:00Z"
        }],
        "available_connections": [
            summary(PROJECT_CONNECTION, "forgejo", project_owner(), true),
            summary(OTHER_CONNECTION, "receiver", team_owner(), false),
        ],
        "can_edit": true, "can_manage_integrations": true
    })
}

async fn mount_task_repository(server: &MockServer, report_status: bool, current: Option<Value>) {
    Mock::given(method("GET"))
        .and(path(REPOSITORY))
        .respond_with(ok(task_repository(report_status, current)))
        .mount(server)
        .await;
}

async fn mount_task_repository_patch(server: &MockServer, body: Value, expected_calls: u64) {
    Mock::given(method("PATCH"))
        .and(path(REPOSITORY))
        .and(body_json(body))
        .respond_with(ok(task_repository(false, None)))
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn test_task_repository_show_prints_connection_events_status_posts_and_choices() {
    let server = MockServer::start().await;
    mount_task_repository(
        &server,
        true,
        Some(summary(
            PROJECT_CONNECTION,
            "forgejo",
            project_owner(),
            true,
        )),
    )
    .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(&home, false, &["task", "repository", "show"]));
    for expected in [
        "FORGEJO",
        "Commit Status: failing, bad token",
        "gradient task repository check-commit-status",
        "Commit status report: on",
        "https://gradient.example/api/v1/hooks/forgejo",
        "push 202 queued",
        "evaluation.completed failed 401: bad token",
        "receiver (RECEIVER)",
    ] {
        assert!(output.contains(expected), "{expected} missing in {output}");
    }
}

#[tokio::test]
async fn test_task_repository_set_connection_by_name_keeps_report_status_when_supported() {
    let server = MockServer::start().await;
    mount_task_repository(
        &server,
        true,
        Some(summary(OTHER_CONNECTION, "receiver", team_owner(), false)),
    )
    .await;
    mount_task_repository_patch(
        &server,
        json!({"git_host_connection": PROJECT_CONNECTION}),
        1,
    )
    .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "task",
            "repository",
            "set-connection",
            "--connection",
            "forgejo",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_task_repository_set_connection_leaves_report_status_to_the_server() {
    let server = MockServer::start().await;
    mount_task_repository(
        &server,
        true,
        Some(summary(
            PROJECT_CONNECTION,
            "forgejo",
            project_owner(),
            true,
        )),
    )
    .await;
    mount_task_repository_patch(&server, json!({"git_host_connection": OTHER_CONNECTION}), 1).await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "task",
            "repository",
            "set-connection",
            "--connection",
            OTHER_CONNECTION,
        ],
    )
    .success();
}

#[tokio::test]
async fn test_task_repository_set_report_status_alone_keeps_the_current_connection() {
    let server = MockServer::start().await;
    mount_task_repository(
        &server,
        false,
        Some(summary(
            PROJECT_CONNECTION,
            "forgejo",
            project_owner(),
            true,
        )),
    )
    .await;
    mount_task_repository_patch(&server, json!({"report_status": true}), 1).await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "task",
            "repository",
            "set-connection",
            "--report-status",
            "true",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_task_repository_set_no_connection_sends_null_and_stops_reports() {
    let server = MockServer::start().await;
    mount_task_repository(
        &server,
        true,
        Some(summary(
            PROJECT_CONNECTION,
            "forgejo",
            project_owner(),
            true,
        )),
    )
    .await;
    mount_task_repository_patch(&server, json!({"git_host_connection": null}), 1).await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &["task", "repository", "set-connection", "--no-connection"],
    )
    .success();
}

#[tokio::test]
async fn test_task_repository_set_connection_refuses_unknown_name_and_conflicting_flags() {
    let server = MockServer::start().await;
    mount_task_repository(&server, false, None).await;
    Mock::given(method("PATCH"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "task",
            "repository",
            "set-connection",
            "--connection",
            "gitlab",
        ],
    )
    .code(2);
    gradient(
        &home,
        true,
        &[
            "task",
            "repository",
            "set-connection",
            "--connection",
            "forgejo",
            "--no-connection",
        ],
    )
    .code(2);
    gradient(&home, true, &["task", "repository", "set-connection"]).code(2);
}

#[tokio::test]
async fn test_task_repository_check_commit_status_posts_the_status_test() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{REPOSITORY}/status-test")))
        .respond_with(ok(json!({
            "id": ATTEMPT, "event": "test", "success": true, "response_status": 201,
            "delivered_at": "2026-11-11T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        true,
        &["task", "repository", "check-commit-status"],
    ));
    let envelope: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(envelope["message"]["success"], true);
}

#[tokio::test]
async fn test_task_repository_install_webhook_and_deploy_key_post_without_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REPOSITORY))
        .respond_with(ok(task_repository(false, None)))
        .expect(0)
        .mount(&server)
        .await;
    for endpoint in ["install-webhook", "install-deploy-key"] {
        Mock::given(method("POST"))
            .and(path(format!("{REPOSITORY}/{endpoint}")))
            .and(body_string(""))
            .respond_with(ok(json!("org/repo")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let home = home_with(&server);
    gradient(&home, true, &["task", "repository", "install-webhook"]).success();
    gradient(&home, true, &["task", "repository", "install-deploy-key"]).success();
}

#[tokio::test]
async fn test_git_host_connection_create_for_github_sends_the_installation_id() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/admin/git-host-connections"))
        .and(body_json(json!({
            "name": "github-acme", "git_host_type": "github", "installation_id": 42
        })))
        .respond_with(ok(connection(OTHER_CONNECTION, None)))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "create",
            "--owner",
            "instance",
            "--name",
            "github-acme",
            "--host-type",
            "github",
            "--installation-id",
            "42",
        ],
    )
    .success();
}

#[tokio::test]
async fn test_git_host_connection_create_for_github_without_installation_id_names_the_flag() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let assert = gradient(
        &home,
        true,
        &[
            "project",
            "git-host-connection",
            "create",
            "--owner",
            "instance",
            "--name",
            "github-acme",
            "--host-type",
            "github",
        ],
    )
    .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("--installation-id"), "{stderr}");
}

#[tokio::test]
async fn test_git_host_connection_list_shows_health_tasks_and_declarative_state() {
    let server = MockServer::start().await;
    let mut declared = summary(PROJECT_CONNECTION, "forgejo", project_owner(), true);
    declared["managed"] = json!(true);
    declared["task_count"] = json!(2);
    declared["last_delivery"] = json!({
        "id": DELIVERY, "event": "push", "status_code": 202, "result": "queued",
        "tasks_scanned": 1, "queued": 1, "created_at": "2026-11-11T00:00:00Z"
    });
    Mock::given(method("GET"))
        .and(path(CONNECTIONS))
        .respond_with(ok(json!([declared])))
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &["project", "git-host-connection", "list"],
    ));
    for expected in ["2 tasks", "declarative state", "last event push queued"] {
        assert!(output.contains(expected), "{expected} missing in {output}");
    }
}

#[tokio::test]
async fn test_git_host_connection_test_prints_the_account_and_fails_on_a_host_error() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CONNECTIONS}/{PROJECT_CONNECTION}/test")))
        .respond_with(ok(json!({"success": true, "account": "gradient-bot"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{CONNECTIONS}/{OTHER_CONNECTION}/test")))
        .respond_with(ok(json!({"success": false, "error": "401 Unauthorized"})))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &["project", "git-host-connection", "test", "forgejo"],
    ));
    assert!(output.contains("gradient-bot"), "{output}");
    let assert = gradient(
        &home,
        false,
        &["project", "git-host-connection", "test", OTHER_CONNECTION],
    )
    .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("401 Unauthorized"), "{stderr}");
}

#[tokio::test]
async fn test_git_host_connection_tasks_lists_the_tasks_using_it() {
    let server = MockServer::start().await;
    mount_project_connections_with_same_name_in_team(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CONNECTIONS}/{PROJECT_CONNECTION}/tasks")))
        .respond_with(ok(json!([{
            "id": ATTEMPT, "project": "web", "name": "app", "display_name": "App",
            "repository": "https://git.example.com/org/app.git", "report_status": true
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    let output = stdout_of(gradient(
        &home,
        false,
        &["project", "git-host-connection", "tasks", "forgejo"],
    ));
    assert!(output.contains("web/app"), "{output}");
    assert!(
        output.contains("https://git.example.com/org/app.git"),
        "{output}"
    );
}
