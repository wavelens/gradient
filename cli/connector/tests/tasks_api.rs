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
                "items": [{"id": "p1", "name": "proj"}],
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
                    "id": "ep-1", "build_id": "b-1", "eval": "packages.x86_64-linux.hello",
                    "derivation_path": "abc-hello.drv", "build_status": "Completed"
                }],
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
    assert_eq!(page.entry_points[0].build_status, "Completed");
}
