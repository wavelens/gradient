use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_caches_returns_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "items": [{"id": "c1", "name": "my-cache"}],
            "total": 1, "page": 1, "per_page": 10
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let res = client.caches().list().await.unwrap();
    assert_eq!(res.items.len(), 1);
}

#[tokio::test]
async fn get_cache_stats_returns_stats() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/my-cache/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "nar_bytes_served": 12345, "hits": 42
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let stats = client.caches().stats("my-cache").await.unwrap();
    assert_eq!(stats.hits, Some(42));
}
