use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_webhooks_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/webhook/my-org"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let webhooks = client.webhooks().list("my-org").await.unwrap();
    assert!(webhooks.is_empty());
}

#[tokio::test]
async fn get_webhook_returns_webhook() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/webhook/my-org/w1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "w1", "organization": "org-id", "name": "My Hook",
            "url": "https://example.com/hook", "events": ["build.failed"],
            "active": true, "created_by": "u1", "created_at": "2024-01-01T00:00:00Z"
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let wh = client.webhooks().get("my-org", "w1").await.unwrap();
    assert_eq!(wh.id, "w1");
}
