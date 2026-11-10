// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

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
        .upload_blobs(
            "sess-1",
            vec![("hash-a".into(), bytes::Bytes::from_static(b"blob-content"))],
        )
        .await
        .unwrap();
    assert_eq!(resp.uploaded, 118);
    assert_eq!(resp.remaining, 0);
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("filename=\"hash-a\""), "{body}");
    assert!(body.contains("blob-content"), "{body}");
}

#[tokio::test]
async fn submit_manifest_returns_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/build-requests/manifest"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "session": "00000000-0000-0000-0000-000000000001", "missing": []
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
        .submit_manifest(connector::api::ManifestRequest {
            project: "my-project".into(),
            files: vec![],
        })
        .await
        .unwrap();
    assert_eq!(
        session.session.to_string(),
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(session.missing.is_empty());
}
