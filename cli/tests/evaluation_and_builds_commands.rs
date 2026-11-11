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
use std::path::Path;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn run_gradient_json_with_temp_home_for_wiremock_in_dir(
    server: &MockServer,
    dir: &Path,
    args: &[&str],
) -> assert_cmd::assert::Assert {
    let home = dir.join("home");
    fs::create_dir_all(home.join("gradient")).unwrap();
    fs::write(
        home.join("gradient/config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();
    Command::cargo_bin("gradient")
        .unwrap()
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .arg("--json")
        .args(args)
        .assert()
}

fn parse_gradient_json_stdout_as_json_value(
    assert: &assert_cmd::assert::Assert,
) -> serde_json::Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

#[tokio::test]
async fn retry_evaluation_prints_new_evaluation_uuid_in_json_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/evals/6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f/retry",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false, "message": "eval-2"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &[
            "evaluation",
            "retry",
            "6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f",
        ],
    )
    .success();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert),
        serde_json::json!({"error": false, "message": "eval-2"})
    );
}

#[tokio::test]
async fn evaluation_report_default_options_without_worker_fleet_saves_output_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/evals/6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f/report",
        ))
        .and(query_param("anonymize_identities", "true"))
        .and(query_param("anonymize_packages", "false"))
        .and(query_param("include_logs", "true"))
        .and(query_param("include_instance", "false"))
        .and(query_param("include_exec", "false"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Disposition", "attachment; filename=\"server.db\"")
                .set_body_bytes(b"SQLite format 3".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &[
            "evaluation",
            "report",
            "6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f",
            "--output-file",
            "mine.db",
        ],
    )
    .success();
    assert_eq!(
        fs::read(dir.path().join("mine.db")).unwrap(),
        b"SQLite format 3"
    );
    assert!(!dir.path().join("server.db").exists());
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert)["message"]["bytes"],
        15
    );
}

#[tokio::test]
async fn gradient_builds_sbom_saves_suggested_file_name_and_prints_path_and_size_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/builds/7e2a1d3b-4c5f-4d6e-9f0a-1b2c3d4e5f60/sbom",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"../hello.cdx.json\"",
                )
                .set_body_bytes(br#"{"bomFormat":"CycloneDX"}"#.to_vec()),
        )
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "sbom", "7e2a1d3b-4c5f-4d6e-9f0a-1b2c3d4e5f60"],
    )
    .success();
    assert_eq!(
        fs::read_to_string(dir.path().join("hello.cdx.json")).unwrap(),
        r#"{"bomFormat":"CycloneDX"}"#
    );
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert)["message"],
        serde_json::json!({"path": "hello.cdx.json", "bytes": 25})
    );
}

#[tokio::test]
async fn abort_build_reports_server_message_and_conflict_exits_nonzero() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/builds/7e2a1d3b-4c5f-4d6e-9f0a-1b2c3d4e5f60/abort",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false, "message": "Build aborted"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/builds/8f3b2e4c-5d6a-4e7f-8a9b-2c3d4e5f6071/abort",
        ))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": true, "message": "Evaluation has finished"
        })))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let aborted = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "abort", "7e2a1d3b-4c5f-4d6e-9f0a-1b2c3d4e5f60"],
    )
    .success();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&aborted)["message"],
        "Build aborted"
    );

    let conflict = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "abort", "8f3b2e4c-5d6a-4e7f-8a9b-2c3d4e5f6071"],
    )
    .failure();
    let envelope = parse_gradient_json_stdout_as_json_value(&conflict);
    assert_eq!(envelope["error"], true);
    assert!(
        envelope["message"]
            .as_str()
            .unwrap()
            .contains("Evaluation has finished")
    );
}

#[tokio::test]
async fn logs_exits_non_zero_when_the_log_stream_cannot_be_opened() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/evals/00000000-0000-0000-0000-000000000404/builds",
        ))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": true, "message": "Evaluation not found"
        })))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let failed = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["logs", "00000000-0000-0000-0000-000000000404"],
    )
    .failure();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&failed)["error"],
        true
    );
}

#[tokio::test]
async fn a_quiet_build_outside_a_repository_reports_the_failure_as_json() {
    let server = MockServer::start().await;
    let dir = TempDir::new().unwrap();

    let failed = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["--project", "p", "build", "--quiet"],
    )
    .failure()
    .code(1);
    let envelope = parse_gradient_json_stdout_as_json_value(&failed);
    assert_eq!(envelope["error"], true);
    assert!(
        envelope["message"]
            .as_str()
            .unwrap()
            .contains("Not in a git repository")
    );
}

const WAITED_EVALUATION: &str = "9a4c3f5d-6e7b-4f80-9b1c-3d4e5f607182";

fn build_evaluation_response_json_with_status(status: &str) -> serde_json::Value {
    serde_json::json!({"error": false, "message": {
        "id": WAITED_EVALUATION,
        "task": "0b5d4a6e-7f8c-4091-8c2d-4e5f60718293", "project_name": "proj",
        "repository": "r", "commit": "c", "wildcard": "*", "status": status,
        "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00",
        "error": null, "entry_points": [], "error_count": 0, "expected_thunks": null,
        "finished_at": null, "next": null, "previous": null, "prioritized": false,
        "progress": null, "started_at": null, "task_display_name": null, "task_name": null,
        "trigger": null, "triggered_by": null, "waiting_reason": null, "warning_count": 0,
        "phase_started_at": {}
    }})
}

async fn mount_evaluation_with_status(server: &MockServer, status: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/evals/{WAITED_EVALUATION}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(build_evaluation_response_json_with_status(status)),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn evaluation_wait_exits_with_code_for_each_end_status_and_prints_status_json() {
    for (status, code) in [("Completed", 0), ("Failed", 1), ("Aborted", 4)] {
        let server = MockServer::start().await;
        mount_evaluation_with_status(&server, status).await;
        let dir = TempDir::new().unwrap();

        let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
            &server,
            dir.path(),
            &["evaluation", "wait", WAITED_EVALUATION],
        )
        .code(code);
        assert_eq!(
            parse_gradient_json_stdout_as_json_value(&assert)["message"],
            serde_json::json!({
                "evaluation": WAITED_EVALUATION, "status": status, "error": null,
                "abort_reason": null, "aborted_by": null, "superseded_by": null,
                "expected_duration_ms": null
            })
        );
    }
}

#[tokio::test]
async fn evaluation_wait_names_the_evaluation_that_replaced_an_aborted_one() {
    let replacement = "970223d8-4c52-46d7-b8fd-0d0a900facb3";
    let mut body = build_evaluation_response_json_with_status("Aborted");
    body["message"]["abort_reason"] = "superseded".into();
    body["message"]["superseded_by"] = replacement.into();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/evals/{WAITED_EVALUATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(home.join("gradient")).unwrap();
    fs::write(
        home.join("gradient/config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();

    let assert = Command::cargo_bin("gradient")
        .unwrap()
        .current_dir(dir.path())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args(["evaluation", "wait", WAITED_EVALUATION])
        .assert()
        .code(4);
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains(&format!(
            "Evaluation {WAITED_EVALUATION}: Aborted (A newer push to the branch replaced this evaluation with {replacement})"
        )),
        "{stdout}"
    );
}

#[tokio::test]
async fn builds_log_with_attempt_prints_the_log_of_that_attempt() {
    let build = "01a05a38-3276-7252-bc05-c139d9c8a015";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/builds/{build}/log")))
        .and(query_param("format", "plain"))
        .and(query_param("attempt", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"error": false, "message": "first try\n"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "log", build, "--attempt", "1"],
    )
    .success();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert)["message"],
        "first try\n"
    );
}

#[tokio::test]
async fn evaluation_wait_with_zero_timeout_exits_with_code_5() {
    let server = MockServer::start().await;
    mount_evaluation_with_status(&server, "Building").await;
    let dir = TempDir::new().unwrap();

    run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["evaluation", "wait", WAITED_EVALUATION, "--timeout", "0"],
    )
    .code(5);
}

#[tokio::test]
async fn watch_without_terminal_prints_status_lines_and_exits_with_evaluation_result_code() {
    let server = MockServer::start().await;
    mount_evaluation_with_status(&server, "Failed").await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["watch", WAITED_EVALUATION],
    )
    .code(1);
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert)["message"]["status"],
        "Failed"
    );
}

#[tokio::test]
async fn task_evaluate_json_prints_evaluation_id_name_and_url() {
    let created = serde_json::json!({
        "id": WAITED_EVALUATION,
        "name": "proj/nightly",
        "url": format!("https://ci.example/project/proj/log/{WAITED_EVALUATION}"),
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/evaluate"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"error": false, "message": created})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let assert = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["--task", "proj/nightly", "task", "evaluate"],
    )
    .success();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&assert)["message"],
        created
    );
}
