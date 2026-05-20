use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_admin_workers_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/workers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).token("t").build().unwrap();
    let workers = client.admin().workers().await.unwrap();
    assert!(workers.is_empty());
}
