use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_projects_returns_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/my-org"))
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
    let res = client.projects().list("my-org").await.unwrap();
    assert_eq!(res.items.len(), 1);
}

#[tokio::test]
async fn badge_returns_svg_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/org/proj/badge"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<svg>ok</svg>"))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let svg = client.projects().badge("org", "proj").await.unwrap();
    assert!(svg.contains("<svg>"));
}
