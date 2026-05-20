use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn submit_manifest_returns_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/build-requests/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "session": "sess-1", "missing": []
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let session = client.build_requests().submit_manifest(connector::build_requests::BuildManifestRequest {
        organization: "my-org".into(),
        files: vec![],
    }).await.unwrap();
    assert_eq!(session.session, "sess-1");
    assert!(session.missing.is_empty());
}
