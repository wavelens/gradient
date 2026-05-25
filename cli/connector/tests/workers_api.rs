use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_workers_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/my-org/workers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let workers = client.workers().list("my-org").await.unwrap();
    assert!(workers.is_empty());
}

#[tokio::test]
async fn create_worker_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs/my-org/workers"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "peer_id": "worker-1", "token": "tok123"
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client
        .workers()
        .create(
            "my-org",
            connector::workers::MakeWorkerRequest {
                worker_id: "worker-1".into(),
                display_name: "My Worker".into(),
                url: None,
                token: None,
                enable_fetch: None,
                enable_eval: None,
                enable_build: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.peer_id, "worker-1");
}
