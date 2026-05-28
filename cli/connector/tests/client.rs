use connector::{Client, ConnectorError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn builder_requires_base_url() {
    let res = Client::builder().build();
    assert!(res.is_err());
}

// Regression for #287: `Client::builder().build()` must succeed regardless of
// whether the platform CA store is reachable. Native certs are loaded on top
// of bundled Mozilla roots so self-hosted instances with self-signed CAs
// installed in the system trust store work; `webpki-roots` remains a fallback
// so the CLI still builds in Nix sandboxes and minimal containers without
// `/etc/ssl/certs`.
#[tokio::test]
async fn builder_succeeds_without_system_certs() {
    Client::builder()
        .base_url("http://localhost")
        .build()
        .expect("client builds with bundled TLS roots");
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
