use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn upload_blobs_decodes_counts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/build-requests/sess-1/blobs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok(serde_json::json!({ "uploaded": 118, "remaining": 0 }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let resp = client
        .build_requests()
        .upload_blobs("sess-1", reqwest::multipart::Form::new())
        .await
        .unwrap();
    assert_eq!(resp.uploaded, 118);
    assert_eq!(resp.remaining, 0);
}

#[tokio::test]
async fn upload_source_nar_returns_dispatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/build-requests/source"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "evaluation": "eval-1", "project": "proj-1", "commit": "commit-1", "cache": "my-cache"
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let resp = client
        .build_requests()
        .upload_source_nar("my-org", Some("pkg"), Some("x86_64-linux"), b"nar".to_vec())
        .await
        .unwrap();
    assert_eq!(resp.evaluation, "eval-1");
    assert_eq!(resp.cache.as_deref(), Some("my-cache"));
}

#[tokio::test]
async fn submit_manifest_returns_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/build-requests/manifest"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "session": "sess-1", "missing": []
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let session = client
        .build_requests()
        .submit_manifest(connector::build_requests::BuildManifestRequest {
            organization: "my-org".into(),
            files: vec![],
        })
        .await
        .unwrap();
    assert_eq!(session.session, "sess-1");
    assert!(session.missing.is_empty());
}
