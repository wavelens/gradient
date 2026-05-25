use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_user_returns_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "u1", "username": "alice", "name": "Alice", "email": "alice@example.com", "superuser": false
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let user = client.user().get().await.unwrap();
    assert_eq!(user.username, "alice");
}

#[tokio::test]
async fn list_keys_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let keys = client.user().keys().await.unwrap();
    assert!(keys.is_empty());
}
