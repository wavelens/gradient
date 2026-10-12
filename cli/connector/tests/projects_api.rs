use connector::Client;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_projects_returns_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "items": [{
                "id": "00000000-0000-0000-0000-000000000001", "name": "foo", "display_name": "Foo", "description": "",
                "created_by": "00000000-0000-0000-0000-000000000002", "created_at": "2024-01-01T00:00:00",
                "hide_build_requests": false, "managed": false, "public": false,
                "public_key": null, "role": null, "running_evaluations": 0
            }],
            "total": 1, "page": 1, "per_page": 10
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.projects().list().await.unwrap();
    assert_eq!(res.items.len(), 1);
}

#[tokio::test]
async fn create_project_sends_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects"))
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
        .projects()
        .create(connector::api::MakeProjectRequest {
            name: "n".to_string().into(),
            display_name: "d".to_string().into(),
            description: "x".into(),
            hide_build_requests: None,
            public: None,
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
    let err = client.projects().list().await.unwrap_err();
    assert!(matches!(err, connector::ConnectorError::Unauthorized));
}
