// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

mod connector_test_websocket_server;

use connector::evals::EvaluationReportQueryParameters;
use connector::{Client, LiveUpdateEvent};
use connector_test_websocket_server::start_local_websocket_server_with_text_frames_returning_http_base_url;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_eval_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/00000000-0000-0000-0000-000000000001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000001", "task": "00000000-0000-0000-0000-000000000002", "project_name": "proj", "repository": "r", "commit": "c1",
                "wildcard": "*", "status": "Completed", "previous": null, "next": null,
                "created_at": "2024-01-01T00:00:00", "updated_at": "2024-01-01T00:00:00",
                "error": null, "entry_points": [], "error_count": 0, "expected_thunks": null,
                "finished_at": null, "prioritized": false, "progress": null, "started_at": null,
                "task_display_name": null, "task_name": null, "trigger": null,
                "triggered_by": null, "waiting_reason": null, "warning_count": 0
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let eval = client
        .evals()
        .get("00000000-0000-0000-0000-000000000001")
        .await
        .unwrap();
    assert_eq!(eval.id.to_string(), "00000000-0000-0000-0000-000000000001");
}

#[tokio::test]
async fn abort_posts_the_abort_method() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/evals/eval-1"))
        .and(body_json(serde_json::json!({"method": "abort"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Success")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    assert_eq!(client.evals().abort("eval-1").await.unwrap(), "Success");
}

#[tokio::test]
async fn stream_builds_outlives_the_request_timeout() {
    use futures::StreamExt;
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let _ = conn.read(&mut [0u8; 4096]).unwrap();
        let chunk = |s: &str| format!("{:x}\r\n{s}\r\n", s.len());
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n";
        conn.write_all(format!("{head}{}", chunk("\"first\"\n")).as_bytes())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        conn.write_all(format!("{}0\r\n\r\n", chunk("\"second\"\n")).as_bytes())
            .unwrap();
    });

    let client = Client::builder()
        .base_url(format!("http://{addr}"))
        .token("t")
        .timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    let lines: Vec<String> = client
        .evals()
        .stream_builds("eval-1")
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(lines, ["first", "second"]);
}

#[expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
fn build_api_client_for_wiremock_server(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap()
}

#[tokio::test]
async fn retry_eval_posts_retry_endpoint_and_returns_new_eval_uuid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/evals/eval-1/retry"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok("00000000-0000-0000-0000-000000000002")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let new_id = build_api_client_for_wiremock_server(&server)
        .evals()
        .retry("eval-1")
        .await
        .unwrap();
    assert_eq!(new_id, "00000000-0000-0000-0000-000000000002");
}

#[tokio::test]
async fn test_prioritize_evaluation_posts_prioritize_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/evals/eval-1/prioritize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Success")))
        .expect(1)
        .mount(&server)
        .await;

    let message = build_api_client_for_wiremock_server(&server)
        .evals()
        .prioritize("eval-1")
        .await
        .unwrap();
    assert_eq!(message, "Success");
}

#[tokio::test]
async fn eval_failure_summary_decodes_from_summary_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "compared_with": {"id": "00000000-0000-0000-0000-000000000002", "commit": "abc"},
            "packages": {"total": 3, "built": 1, "failed": 1, "blocked": 1, "unfinished": 0},
            "failures": [{
                "build_id": "00000000-0000-0000-0000-000000000003", "name": "hello", "derivation_path": "/nix/store/x-hello.drv",
                "architecture": "x86_64-linux", "status": "FailedPermanent",
                "attributes": ["packages.x86_64-linux.hello"], "blocked": ["world"],
                "blocked_total": 1, "newly_failing": true
            }],
            "failures_total": 1,
            "failed_attributes": [{"attr": "checks.broken", "message": "boom", "newly_failing": false}],
            "fixed": ["old"],
            "fixed_total": 1,
            "unfinished_attributes": []
        }))))
        .mount(&server)
        .await;

    let summary = build_api_client_for_wiremock_server(&server)
        .evals()
        .summary("eval-1")
        .await
        .unwrap();
    assert_eq!(summary.packages.failed, 1);
    assert_eq!(summary.failures[0].name, "hello");
    assert!(summary.failures[0].newly_failing);
    assert_eq!(summary.failed_attributes[0].attr, "checks.broken");
    assert_eq!(summary.compared_with.unwrap().commit, "abc");
}

#[tokio::test]
async fn evaluation_report_download_returns_file_name_from_content_disposition_with_all_report_options_as_query_parameters()
 {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/report"))
        .and(query_param("anonymize_identities", "true"))
        .and(query_param("anonymize_packages", "false"))
        .and(query_param("include_logs", "true"))
        .and(query_param("include_instance", "false"))
        .and(query_param("include_exec", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"gradient-report-eval.db\"",
                )
                .set_body_bytes(b"SQLite format 3".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let file = build_api_client_for_wiremock_server(&server)
        .evals()
        .report(
            "eval-1",
            &EvaluationReportQueryParameters {
                anonymize_identities: true,
                anonymize_packages: false,
                include_logs: true,
                include_instance: false,
                include_exec: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        file.suggested_filename.as_deref(),
        Some("gradient-report-eval.db")
    );
    assert_eq!(&file.bytes[..], b"SQLite format 3");
}

fn closure_graph(root: &str) -> serde_json::Value {
    serde_json::json!({
        "roots": [root], "total_size_bytes": 2048, "node_count": 1, "edge_count": 0,
        "truncated": false, "nodes": [{"id": root, "name": "hello", "path": "/nix/store/x-hello", "nar_size": 2048}],
        "edges": []
    })
}

#[tokio::test]
async fn evaluation_closure_and_runtime_closure_read_their_own_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/closure"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(closure_graph("build-time"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/runtime-closure"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(closure_graph("runtime"))))
        .mount(&server)
        .await;

    let evals = build_api_client_for_wiremock_server(&server);
    let build_time = evals.evals().closure("eval-1").await.unwrap();
    let runtime = evals.evals().runtime_closure("eval-1").await.unwrap();
    assert_eq!(build_time.roots, ["build-time"]);
    assert_eq!(runtime.roots, ["runtime"]);
    assert_eq!(runtime.total_size_bytes, Some(2048));
}

#[tokio::test]
async fn test_evaluation_flake_output_graph_is_decoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/flake-graph"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([
            {"path": "packages", "parent": null, "name": "packages", "kind": "attrset", "is_derivation": false, "drv_path": null},
            {"path": "packages.x86_64-linux.hello", "parent": "packages.x86_64-linux", "name": "hello",
             "kind": "derivation", "is_derivation": true, "drv_path": "/nix/store/x-hello.drv"}
        ]))))
        .mount(&server)
        .await;

    let nodes = build_api_client_for_wiremock_server(&server)
        .evals()
        .flake_graph("eval-1")
        .await
        .unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[1].drv_path.as_deref(), Some("/nix/store/x-hello.drv"));
}

#[tokio::test]
async fn evaluation_live_websocket_yields_parsed_text_frames_with_bearer_token() {
    use futures::StreamExt;

    let (base_url, request) =
        start_local_websocket_server_with_text_frames_returning_http_base_url(vec![
            r#"{"event":"evaluation.building","at":"2026-11-09T00:00:00Z","content":{"status":3}}"#
                .to_owned(),
        ])
        .await;
    let client = Client::builder()
        .base_url(base_url)
        .token("t")
        .build()
        .unwrap();
    let events: Vec<LiveUpdateEvent> = client
        .evals()
        .live("eval-1")
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "evaluation.building");
    assert_eq!(events[0].content["status"], 3);
    let (request_path, authorization) = request.await.unwrap();
    assert_eq!(request_path, "/api/v1/evals/eval-1/live");
    assert_eq!(authorization.as_deref(), Some("Bearer t"));
}
