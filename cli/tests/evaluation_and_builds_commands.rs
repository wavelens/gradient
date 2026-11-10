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
        .and(path("/api/v1/evals/eval-1/retry"))
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
        &["evaluation", "retry", "eval-1"],
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
        .and(path("/api/v1/evals/eval-1/report"))
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
        &["evaluation", "report", "eval-1", "--output-file", "mine.db"],
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
        .and(path("/api/v1/builds/b1/sbom"))
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
        &["builds", "sbom", "b1"],
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
        .and(path("/api/v1/builds/b1/abort"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false, "message": "Build aborted"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/builds/done/abort"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": true, "message": "Evaluation has finished"
        })))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let aborted = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "abort", "b1"],
    )
    .success();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&aborted)["message"],
        "Build aborted"
    );

    let conflict = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["builds", "abort", "done"],
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
        .and(path("/api/v1/evals/missing/builds"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": true, "message": "Evaluation not found"
        })))
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();

    let failed = run_gradient_json_with_temp_home_for_wiremock_in_dir(
        &server,
        dir.path(),
        &["logs", "missing"],
    )
    .failure();
    assert_eq!(
        parse_gradient_json_stdout_as_json_value(&failed)["error"],
        true
    );
}
