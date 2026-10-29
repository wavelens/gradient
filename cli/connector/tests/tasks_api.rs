use connector::Client;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_tasks_returns_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/my-project"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000001", "name": "proj", "display_name": "Proj", "description": "",
                    "project": "00000000-0000-0000-0000-000000000002", "repository": "r", "wildcard": "*", "active": true,
                    "created_by": "00000000-0000-0000-0000-000000000003", "created_at": "2024-01-01T00:00:00", "managed": false,
                    "can_edit": true, "can_trigger": true, "concurrency": "skip",
                    "force_evaluation": false, "keep_evaluations": 30, "last_evaluation": null,
                    "last_evaluation_status": null, "retry_failed_builds": false, "multi_node_evaluation": false, "exec_enabled": false,
                    "sign_cache": true, "wait_for_workers": false
                }],
                "total": 1, "page": 1, "per_page": 10
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.tasks().list("my-project").await.unwrap();
    assert_eq!(res.items.len(), 1);
}

#[tokio::test]
async fn badge_returns_svg_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/project/proj/badge"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<svg>ok</svg>"))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let svg = client.tasks().badge("project", "proj").await.unwrap();
    assert!(svg.contains("<svg>"));
}

#[tokio::test]
async fn evaluate_pins_a_commit() {
    let server = MockServer::start().await;
    let commit = "9c1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3";
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/evaluate"))
        .and(body_json(serde_json::json!({"commit": commit})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("eval-9")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let id = client
        .tasks()
        .evaluate("proj", "nightly", Some(commit))
        .await
        .unwrap();
    assert_eq!(id, "eval-9");
}

#[tokio::test]
async fn evaluate_without_a_commit_sends_no_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/evaluate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("eval-9")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .tasks()
        .evaluate("proj", "nightly", None)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].body.is_empty(), "unexpected body");
}

#[tokio::test]
async fn entry_points_filter_by_evaluation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/entry-points"))
        .and(query_param("evaluation_id", "eval-1"))
        .and(query_param("limit", "500"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "entry_points": [{
                    "id": "00000000-0000-0000-0000-000000000001", "build_id": "00000000-0000-0000-0000-000000000002", "eval": "packages.x86_64-linux.hello",
                    "derivation_path": "abc-hello.drv", "build_status": "Completed",
                    "architecture": "x86_64-linux", "build_started_at": null,
                    "build_time_ms": null, "created_at": "2024-01-01T00:00:00",
                    "deps": {
                        "aborted": 0, "building": 0, "completed": 0,
                        "failed": 0, "queued": 0, "substituted": 0
                    },
                    "deps_total": 0, "has_artefacts": false, "ifd": false,
                    "outputs": {}, "prioritized": false
                }],
                "failed_attributes": [],
                "total": 1
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let page = client
        .tasks()
        .entry_points("proj", "nightly", Some("eval-1"), Some(500), None)
        .await
        .unwrap();
    assert_eq!(
        page.entry_points[0].build_status,
        connector::api::BuildStatus::Completed
    );
}
