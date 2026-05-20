use connector::{Client, ConnectorError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn builder_requires_base_url() {
    let res = Client::builder().build();
    assert!(res.is_err());
}

#[tokio::test]
async fn health_succeeds_without_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": "ok"
        })))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let msg = client.health().await.unwrap();
    assert_eq!(msg, "ok");
}

#[tokio::test]
async fn server_error_envelope_returns_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": true,
            "message": "service unavailable"
        })))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let err = client.health().await.unwrap_err();
    match err {
        ConnectorError::Api { message, .. } => assert_eq!(message, "service unavailable"),
        e => panic!("expected Api, got {:?}", e),
    }
}
