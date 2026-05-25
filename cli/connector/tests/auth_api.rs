use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn basic_login_returns_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/basic/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("abc")))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let token = client
        .auth()
        .basic_login(connector::auth::MakeLoginRequest {
            loginname: "user".into(),
            password: "pass".into(),
        })
        .await
        .unwrap();
    assert_eq!(token, "abc");
}

#[tokio::test]
async fn check_username_returns_bool() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/check-username"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(true)))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let available = client.auth().check_username("alice").await.unwrap();
    assert!(available);
}
