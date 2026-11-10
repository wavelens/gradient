// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

mod connector_test_websocket_server;

use connector::{Client, ConnectorError};
use connector_test_websocket_server::start_local_websocket_server_with_text_frames_returning_http_base_url;
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_build_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/00000000-0000-0000-0000-000000000001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001", "evaluation": "00000000-0000-0000-0000-000000000002", "status": "Completed",
            "derivation_path": "/nix/store/abc.drv", "architecture": "x86_64-linux",
            "output": {}, "created_at": "2024-01-01T00:00:00", "updated_at": "2024-01-01T00:00:00",
            "dispatched_job": null, "entry_points": [], "ifd": false, "prioritized": false, "progress": null,
            "worker": null
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let build = client
        .builds()
        .get("00000000-0000-0000-0000-000000000001")
        .await
        .unwrap();
    assert_eq!(build.id.to_string(), "00000000-0000-0000-0000-000000000001");
    assert_eq!(build.status, connector::api::BuildStatus::Completed);
}

#[tokio::test]
async fn download_file_returns_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/download/result.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"binary-data".to_vec()))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let pieces: Vec<_> = client
        .builds()
        .download_file("b1", "result.tar.gz")
        .await
        .unwrap()
        .collect()
        .await;
    let bytes: Vec<u8> = pieces
        .into_iter()
        .flat_map(|piece| piece.unwrap().to_vec())
        .collect();
    assert_eq!(bytes, b"binary-data");
}

#[tokio::test]
async fn a_download_without_progress_fails_after_the_idle_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/download/result.tar.gz"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"binary-data".to_vec())
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap();
    let Err(ConnectorError::Io(error)) = client.builds().download_file("b1", "result.tar.gz").await
    else {
        panic!("a stalled download must fail with a timeout");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
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
async fn build_retry_abort_and_prioritize_post_to_own_endpoint() {
    let server = MockServer::start().await;
    for action in ["retry", "abort", "prioritize"] {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/builds/b1/{action}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok(action)))
            .expect(1)
            .mount(&server)
            .await;
    }

    let builds = build_api_client_for_wiremock_server(&server);
    assert_eq!(builds.builds().retry("b1").await.unwrap(), "retry");
    assert_eq!(builds.builds().abort("b1").await.unwrap(), "abort");
    assert_eq!(
        builds.builds().prioritize("b1").await.unwrap(),
        "prioritize"
    );
}

fn closure_graph(root: &str) -> serde_json::Value {
    serde_json::json!({
        "roots": [root], "total_size_bytes": null, "node_count": 0, "edge_count": 0,
        "truncated": true, "nodes": [], "edges": []
    })
}

#[tokio::test]
async fn build_and_runtime_closures_read_their_own_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/closure"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(closure_graph("build-time"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/runtime-closure"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(closure_graph("runtime"))))
        .mount(&server)
        .await;

    let builds = build_api_client_for_wiremock_server(&server);
    assert_eq!(
        builds.builds().closure("b1").await.unwrap().roots,
        ["build-time"]
    );
    let runtime = builds.builds().runtime_closure("b1").await.unwrap();
    assert_eq!(runtime.roots, ["runtime"]);
    assert!(runtime.truncated);
}

#[tokio::test]
async fn download_build_sbom_returns_bytes_and_content_disposition_filename_or_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/sbom"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"hello.cdx.json\"",
                )
                .set_body_bytes(br#"{"bomFormat":"CycloneDX"}"#.to_vec()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/missing/sbom"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": true, "message": "Build not found"
        })))
        .mount(&server)
        .await;

    let builds = build_api_client_for_wiremock_server(&server);
    let file = builds.builds().sbom("b1").await.unwrap();
    assert_eq!(file.suggested_filename.as_deref(), Some("hello.cdx.json"));
    assert_eq!(&file.bytes[..], br#"{"bomFormat":"CycloneDX"}"#);

    match builds.builds().sbom("missing").await {
        Err(ConnectorError::Api { status, message }) => {
            assert_eq!(status.as_u16(), 404);
            assert_eq!(message, "Build not found");
        }
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[tokio::test]
async fn build_live_websocket_yields_parsed_live_events() {
    use futures::StreamExt;

    let (base_url, request) =
        start_local_websocket_server_with_text_frames_returning_http_base_url(vec![
            r#"{"event":"build.progress","at":"2026-11-09T00:00:00Z","content":{"phase":"upload"}}"#
                .to_owned(),
            r#"{"event":"build.status_changed","at":"2026-11-09T00:00:01Z","content":{"status":4}}"#
                .to_owned(),
        ])
        .await;
    let client = Client::builder().base_url(base_url).build().unwrap();
    let events: Vec<String> = client
        .builds()
        .live("b1")
        .await
        .unwrap()
        .map(|event| event.unwrap().event)
        .collect()
        .await;

    assert_eq!(events, ["build.progress", "build.status_changed"]);
    let (request_path, authorization) = request.await.unwrap();
    assert_eq!(request_path, "/api/v1/builds/b1/live");
    assert_eq!(authorization, None);
}
