/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `gradient mcp` speaks JSON-RPC on stdout, so these drive the real binary
//! over a pipe: anything the command prints outside a frame corrupts the
//! session for every MCP client.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_config(home: &TempDir, body: &str) {
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), body).unwrap();
}

fn frames(requests: &[Value]) -> String {
    let mut buf = String::new();
    for req in requests {
        buf.push_str(&serde_json::to_string(req).unwrap());
        buf.push('\n');
    }
    buf
}

fn initialize() -> Vec<Value> {
    vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "gradient-test", "version": "0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ]
}

fn session(home: &TempDir, requests: &[Value]) -> Vec<Value> {
    session_with(home, &["mcp"], requests)
}

/// Run a session to completion: stdin is closed after the last frame, which
/// ends the server, so the responses are whatever landed on stdout.
fn session_with(home: &TempDir, args: &[&str], requests: &[Value]) -> Vec<Value> {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("TMPDIR", home.path())
        .args(args)
        .write_stdin(frames(requests))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "mcp exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("stdout is utf-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON on stdout: {l:?} ({e})"))
        })
        .collect()
}

fn response(responses: &[Value], id: u64) -> &Value {
    responses
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| panic!("no response for id {id}: {responses:#?}"))
}

fn tool_names(responses: &[Value]) -> Vec<String> {
    response(responses, 2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn call(name: &str, arguments: Value) -> Vec<Value> {
    let mut requests = initialize();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    }));
    requests
}

fn server_config(home: &TempDir, server: &MockServer) {
    write_config(
        home,
        &format!(
            "Server = '{}'\nAuthToken = 'seeded-token'\nSelectedProject = 'proj'\n",
            server.uri()
        ),
    );
}

fn offline_config(home: &TempDir) {
    write_config(
        home,
        "Server = 'http://gradient.invalid'\nAuthToken = 'test-token'\n",
    );
}

#[test]
fn mcp_completes_the_initialize_handshake() {
    let home = TempDir::new().unwrap();
    offline_config(&home);

    let responses = session(&home, &initialize());
    let init = response(&responses, 1);

    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["serverInfo"]["name"], "gradient");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "server must advertise the tools capability: {init:#}"
    );
}

#[test]
fn mcp_advertises_the_read_only_tools() {
    let home = TempDir::new().unwrap();
    offline_config(&home);

    let mut requests = initialize();
    requests.push(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));

    let responses = session(&home, &requests);
    let tools = response(&responses, 2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();

    for expected in [
        "list_projects",
        "list_tasks",
        "list_evaluations",
        "get_evaluation",
        "list_builds",
        "get_build",
        "get_build_log",
        "search_build_log",
    ] {
        assert!(
            tools.contains(&expected.to_string()),
            "missing {expected}: {tools:?}"
        );
    }
}

// Read-only was a deliberate choice: an MCP client must not be able to spend
// the build farm or delete a project through this server.
#[test]
fn mcp_exposes_no_mutating_tools() {
    let home = TempDir::new().unwrap();
    offline_config(&home);

    let mut requests = initialize();
    requests.push(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));

    let responses = session(&home, &requests);
    for tool in response(&responses, 2)["result"]["tools"]
        .as_array()
        .unwrap()
    {
        let name = tool["name"].as_str().unwrap();
        assert!(
            ![
                "create", "delete", "update", "trigger", "evaluate", "abort", "restart"
            ]
            .iter()
            .any(|verb| name.starts_with(verb)),
            "mutating tool exposed: {name}"
        );
    }
}

#[tokio::test]
async fn mcp_get_build_log_sends_the_configured_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/build-1/log/lines"))
        .and(header("Authorization", "Bearer seeded-token"))
        .and(query_param("start", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("error: builder failed\n"))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(
        &home,
        &format!("Server = '{}'\nAuthToken = 'seeded-token'\n", server.uri()),
    );

    let mut requests = initialize();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "get_build_log", "arguments": {"build": "build-1"}}
    }));

    let responses = session(&home, &requests);
    let result = &response(&responses, 2)["result"];

    assert_eq!(result["isError"], json!(false), "{result:#}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("error: builder failed"),
        "log not returned: {result:#}"
    );
}

#[tokio::test]
async fn mcp_get_build_log_saves_a_long_log_to_a_temp_file() {
    let log = (1..=11).map(|n| format!("line {n}\n")).collect::<String>();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/build-1/log/lines"))
        .respond_with(ResponseTemplate::new(200).set_body_string(log.clone()))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(
        &home,
        &format!("Server = '{}'\nAuthToken = 'seeded-token'\n", server.uri()),
    );

    let mut requests = initialize();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "get_build_log", "arguments": {"build": "build-1"}}
    }));

    let responses = session(&home, &requests);
    let result = &response(&responses, 2)["result"];
    let text = result["content"][0]["text"].as_str().unwrap();

    assert_eq!(result["isError"], json!(false), "{result:#}");
    assert!(!text.contains("line 1\n"), "log inlined: {text}");
    let saved = fs::read_dir(home.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|ext| ext == "log"))
        .unwrap_or_else(|| panic!("no log file saved: {text}"));
    assert!(
        text.contains(saved.to_str().unwrap()),
        "path not returned: {text}"
    );
    assert_eq!(fs::read_to_string(saved).unwrap(), log);
}

// The selected project in config.toml is the default, so an agent does not have
// to name the project on every call.
#[tokio::test]
async fn mcp_list_tasks_defaults_to_the_selected_project() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/sandro"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "items": [{"id": "task-1", "name": "nightly"}],
                "total": 1, "page": 1, "per_page": 20
            }
        })))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(
        &home,
        &format!(
            "Server = '{}'\nAuthToken = 'seeded-token'\nSelectedProject = 'sandro'\n",
            server.uri()
        ),
    );

    let mut requests = initialize();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "list_tasks", "arguments": {}}
    }));

    let responses = session(&home, &requests);
    let result = &response(&responses, 2)["result"];

    assert_eq!(result["isError"], json!(false), "{result:#}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nightly"),
        "task not returned: {result:#}"
    );
}

// An unreachable server is the tool's failure, not the protocol's: it must come
// back as a tool-level error the client can render, not a JSON-RPC error.
#[test]
fn mcp_reports_api_failures_as_tool_errors() {
    let home = TempDir::new().unwrap();
    offline_config(&home);

    let mut requests = initialize();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "list_projects", "arguments": {}}
    }));

    let responses = session(&home, &requests);
    let reply = response(&responses, 2);

    assert!(
        reply["error"].is_null(),
        "must not be a protocol error: {reply:#}"
    );
    assert_eq!(reply["result"]["isError"], json!(true), "{reply:#}");
}

#[test]
fn mcp_without_a_server_url_exits_with_a_usage_error() {
    let home = TempDir::new().unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .arg("mcp")
        .write_stdin(String::new())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("gradient login"));
}

const CONTROL_TOOLS: [&str; 3] = ["start_evaluation", "abort_evaluation", "watch_evaluation"];

#[test]
fn mcp_control_tools_are_opt_in() {
    let home = TempDir::new().unwrap();
    offline_config(&home);
    let mut requests = initialize();
    requests.push(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));

    let plain = tool_names(&session(&home, &requests));
    let control = tool_names(&session_with(&home, &["mcp", "--control"], &requests));

    for tool in CONTROL_TOOLS {
        assert!(
            !plain.contains(&tool.to_string()),
            "{tool} without --control"
        );
        assert!(
            control.contains(&tool.to_string()),
            "{tool} missing with --control"
        );
    }
    assert!(control.contains(&"get_build_log".to_string()));
}

#[tokio::test]
async fn mcp_start_evaluation_returns_the_new_evaluation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/evaluate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false, "message": "eval-9"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let responses = session_with(
        &home,
        &["mcp", "--control"],
        &call("start_evaluation", json!({"task": "nightly"})),
    );
    let result = &response(&responses, 2)["result"];

    assert_eq!(result["isError"], json!(false), "{result:#}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("eval-9")
    );
}

#[tokio::test]
async fn mcp_abort_evaluation_sends_the_abort_method() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/evals/eval-1"))
        .and(body_json(json!({"method": "abort"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false, "message": "Success"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let responses = session_with(
        &home,
        &["mcp", "--control"],
        &call("abort_evaluation", json!({"evaluation": "eval-1"})),
    );

    assert_eq!(response(&responses, 2)["result"]["isError"], json!(false));
}

fn eval_body(status: &str) -> Value {
    json!({"error": false, "message": {
        "id": "eval-1", "task": "t", "repository": "r", "commit": "c",
        "wildcard": "*", "status": status, "created_at": "2026-01-01T00:00:00Z",
        "error": null
    }})
}

fn entry_point(attr: &str, status: &str) -> Value {
    json!({
        "id": format!("ep-{attr}"), "build_id": format!("b-{attr}"), "eval": attr,
        "derivation_path": format!("abc-{attr}.drv"), "build_status": status
    })
}

async fn mount_eval(server: &MockServer, status: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(eval_body(status)))
        .mount(server)
        .await;
}

async fn mount_entry_points(server: &MockServer, offset: &str, items: Vec<Value>, total: u64) {
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/entry-points"))
        .and(query_param("evaluation_id", "eval-1"))
        .and(query_param("offset", offset))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {"entry_points": items, "total": total}
        })))
        .mount(server)
        .await;
}

fn watch_report(home: &TempDir, arguments: Value) -> Value {
    let responses = session_with(
        home,
        &["mcp", "--control"],
        &call("watch_evaluation", arguments),
    );
    let result = &response(&responses, 2)["result"];
    assert_eq!(result["isError"], json!(false), "{result:#}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn mcp_watch_evaluation_reports_entry_points_of_a_finished_run() {
    let server = MockServer::start().await;
    mount_eval(&server, "Failed").await;
    mount_entry_points(
        &server,
        "0",
        vec![entry_point("hello", "FailedPermanent")],
        1,
    )
    .await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let report = watch_report(&home, json!({"task": "nightly", "evaluation": "eval-1"}));

    assert_eq!(report["finished"], json!(true), "{report:#}");
    assert_eq!(report["status"], "Failed");
    assert_eq!(report["entry_points"][0]["attr"], "hello");
    assert_eq!(report["entry_points"][0]["build_status"], "FailedPermanent");
}

#[tokio::test]
async fn mcp_watch_evaluation_returns_unfinished_on_timeout() {
    let server = MockServer::start().await;
    mount_eval(&server, "Building").await;
    mount_entry_points(&server, "0", vec![entry_point("hello", "Building")], 1).await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let report = watch_report(
        &home,
        json!({"task": "nightly", "evaluation": "eval-1", "timeout_seconds": 1}),
    );

    assert_eq!(report["finished"], json!(false), "{report:#}");
    assert_eq!(report["status"], "Building");
}

#[tokio::test]
async fn mcp_watch_evaluation_pages_through_entry_points() {
    let server = MockServer::start().await;
    mount_eval(&server, "Completed").await;
    let first = (0..500)
        .map(|n| entry_point(&format!("p{n}"), "Completed"))
        .collect();
    mount_entry_points(&server, "0", first, 501).await;
    mount_entry_points(&server, "500", vec![entry_point("last", "Completed")], 501).await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let report = watch_report(&home, json!({"task": "nightly", "evaluation": "eval-1"}));

    assert_eq!(report["entry_points"].as_array().unwrap().len(), 501);
    assert_eq!(report["entry_points"][500]["attr"], "last");
}

#[tokio::test]
async fn mcp_watch_evaluation_without_evaluations_is_a_tool_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/evaluations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false, "message": []
        })))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    server_config(&home, &server);

    let responses = session_with(
        &home,
        &["mcp", "--control"],
        &call("watch_evaluation", json!({"task": "nightly"})),
    );

    assert_eq!(response(&responses, 2)["result"]["isError"], json!(true));
}
