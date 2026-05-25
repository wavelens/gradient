use connector::Client;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_orgs_returns_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "items": [{"id": "1", "name": "foo"}],
            "total": 1, "page": 1, "per_page": 10
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.orgs().list().await.unwrap();
    assert_eq!(res.items.len(), 1);
}

#[tokio::test]
async fn create_org_sends_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/orgs"))
        .and(body_json(
            json!({ "name": "n", "display_name": "d", "description": "x" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("created-id")))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let id = client
        .orgs()
        .create(connector::orgs::MakeOrganizationRequest {
            name: "n".into(),
            display_name: "d".into(),
            description: "x".into(),
        })
        .await
        .unwrap();
    assert_eq!(id, "created-id");
}

#[tokio::test]
async fn unauthenticated_client_errors_before_send() {
    let client = Client::builder()
        .base_url("http://example.invalid")
        .build()
        .unwrap();
    let err = client.orgs().list().await.unwrap_err();
    assert!(matches!(err, connector::ConnectorError::Unauthorized));
}
