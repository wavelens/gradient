// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::CreateFlakeInputOverrideRequest;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OVERRIDE_ID: &str = "00000000-0000-0000-0000-000000000001";
const BASE: &str = "/api/v1/tasks/proj/nightly/flake-inputs";

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

fn flake_input(url: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": OVERRIDE_ID, "task": "00000000-0000-0000-0000-000000000002", "input_name": "nixpkgs", "url": url,
        "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00"
    })
}

fn client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap()
}

#[tokio::test]
async fn test_flake_input_override_create_get_list_delete_use_expected_paths() {
    let server = MockServer::start().await;
    let url = "github:NixOS/nixpkgs/nixos-unstable";
    Mock::given(method("POST"))
        .and(path(BASE))
        .and(body_json(
            serde_json::json!({"input_name": "nixpkgs", "url": url}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(flake_input(Some(url)))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(BASE))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok([flake_input(Some(url))])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{BASE}/{OVERRIDE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(flake_input(Some(url)))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{BASE}/{OVERRIDE_ID}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({"deleted": true}))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let flake_inputs = client.flake_inputs();
    let created = flake_inputs
        .create(
            "proj",
            "nightly",
            &CreateFlakeInputOverrideRequest {
                input_name: "nixpkgs".into(),
                url: Some(url.into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(created.url.as_deref(), Some(url));
    assert_eq!(flake_inputs.list("proj", "nightly").await.unwrap().len(), 1);
    assert_eq!(
        flake_inputs
            .get("proj", "nightly", OVERRIDE_ID)
            .await
            .unwrap()
            .input_name,
        "nixpkgs"
    );
    assert!(
        flake_inputs
            .delete("proj", "nightly", OVERRIDE_ID)
            .await
            .unwrap()
            .deleted
    );
}

#[tokio::test]
async fn clearing_flake_input_override_url_sends_explicit_json_null() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("{BASE}/{OVERRIDE_ID}")))
        .and(body_json(serde_json::json!({"url": null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(flake_input(None))))
        .expect(1)
        .mount(&server)
        .await;

    let updated = client(&server)
        .flake_inputs()
        .update("proj", "nightly", OVERRIDE_ID, None, Some(None))
        .await
        .unwrap();
    assert_eq!(updated.url, None);
}

#[tokio::test]
async fn renaming_flake_input_override_leaves_url_out_of_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("{BASE}/{OVERRIDE_ID}")))
        .and(body_json(serde_json::json!({"input_name": "nixpkgs"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(flake_input(None))))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .flake_inputs()
        .update("proj", "nightly", OVERRIDE_ID, Some("nixpkgs"), None)
        .await
        .unwrap();
}
