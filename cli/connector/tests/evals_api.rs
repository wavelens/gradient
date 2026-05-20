use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_eval_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "eval-1", "project": "p1", "repository": "r", "commit": "c1",
            "wildcard": "*", "status": "Completed", "previous": null, "next": null,
            "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z",
            "error": null
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let eval = client.evals().get("eval-1").await.unwrap();
    assert_eq!(eval.id, "eval-1");
}
