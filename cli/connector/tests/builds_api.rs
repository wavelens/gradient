// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_build_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/00000000-0000-0000-0000-000000000001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001", "evaluation": "00000000-0000-0000-0000-000000000002", "status": "Completed",
            "derivation_path": "/nix/store/abc.drv", "architecture": "x86_64-linux",
            "output": {}, "created_at": "2024-01-01T00:00:00", "updated_at": "2024-01-01T00:00:00",
            "dispatched_job": null, "ifd": false, "prioritized": false, "progress": null,
            "worker": null
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let build = client
        .builds()
        .get("00000000-0000-0000-0000-000000000001")
        .await
        .unwrap();
    assert_eq!(build.id.to_string(), "00000000-0000-0000-0000-000000000001");
    assert_eq!(build.status, connector::api::BuildStatus::Completed);
}

#[tokio::test]
async fn download_file_returns_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/download/result.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"binary-data".to_vec()))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let bytes = client
        .builds()
        .download_file("b1", "result.tar.gz")
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"binary-data");
}
