// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_workers_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/my-project/workers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let workers = client.workers().list("my-project").await.unwrap();
    assert!(workers.is_empty());
}

#[tokio::test]
async fn create_worker_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/my-project/workers"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "peer_id": "00000000-0000-0000-0000-000000000001", "token": "tok123"
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client
        .workers()
        .create(
            "my-project",
            connector::api::RegisterWorkerRequest {
                worker_id: "worker-1".into(),
                display_name: "My Worker".into(),
                url: None,
                token: None,
                enable_build: None,
                enable_eval: None,
                enable_exec: None,
                enable_network: None,
                enable_secrets: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        res.peer_id.to_string(),
        "00000000-0000-0000-0000-000000000001"
    );
}

#[tokio::test]
async fn worker_metrics_decode() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/my-project/workers/w1/metrics"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "worker_id": "w1",
                "display_name": "Worker 1",
                "jobs_dispatched": 7,
                "samples": [{
                    "at": "2026-11-09T00:00:00Z",
                    "assigned_jobs": 1,
                    "max_concurrent_builds": 4,
                    "state": 0,
                    "cpu_usage_pct": 12.5
                }],
                "connections": [{"connected_at": "2026-11-09T00:00:00Z"}]
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let metrics = client
        .workers()
        .get_worker_metrics("my-project", "w1")
        .await
        .unwrap();
    assert_eq!(metrics.jobs_dispatched, 7);
    assert_eq!(metrics.samples[0].max_concurrent_builds, 4);
    assert!(metrics.connections[0].disconnected_at.is_none());
}

#[tokio::test]
async fn test_worker_check_posts_to_test_and_decodes_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/my-project/workers/w1/test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "ok": false,
                "connected": true,
                "authorized_for_project": false,
                "message": "not authorized"
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let result = client
        .workers()
        .test_worker_connection_and_authorization("my-project", "w1")
        .await
        .unwrap();
    assert!(result.connected);
    assert!(!result.authorized_for_project);
}
