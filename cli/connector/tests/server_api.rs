use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn config_returns_struct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": {
                "oidc_enabled": false,
                "registration_enabled": true,
                "email_verification_enabled": false,
                "quic": false
            }
        })))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let cfg = client.server().get_config().await.unwrap();
    assert!(!cfg.oidc_enabled);
    assert!(cfg.registration_enabled);
}
