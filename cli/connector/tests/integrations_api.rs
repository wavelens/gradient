use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_integrations_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/my-org/integrations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let integrations = client.integrations().list("my-org").await.unwrap();
    assert!(integrations.is_empty());
}

#[tokio::test]
async fn summary_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/my-org/integrations/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let summaries = client.integrations().summary("my-org").await.unwrap();
    assert!(summaries.is_empty());
}
