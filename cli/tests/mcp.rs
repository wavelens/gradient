/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `gradient mcp` speaks JSON-RPC on stdout, so these drive the real binary
//! over a pipe: anything the command prints outside a frame corrupts the
//! session for every MCP client.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path, query_param};
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

/// Run a session to completion: stdin is closed after the last frame, which
/// ends the server, so the responses are whatever landed on stdout.
fn session(home: &TempDir, requests: &[Value]) -> Vec<Value> {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .arg("mcp")
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
