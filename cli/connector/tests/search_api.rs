// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use connector::api::HitKind;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn search_sends_q_and_limit_and_decodes_hits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("q", "nix"))
        .and(query_param("limit", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": [{
                "kind": "project", "label": "Nix Tools", "sublabel": "nix-tools",
                "route": "/projects/nix-tools", "starred": true
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let hits = client.search().find_by_query("nix", Some(3)).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, HitKind::Project);
    assert!(hits[0].starred);
}
