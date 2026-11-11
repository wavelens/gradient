// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
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
                "allow_exec_in_build_requests": false, "restrict_evaluation_fetches": true, "extra_allowed_uri_prefixes": [], "hide_build_requests": false, "managed": false, "public": false,
                "public_key": null, "role": null, "running_evaluations": 0, "waiting_evaluations": 0, "permissions": []
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

fn project_summary(name: &str) -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-000000000001", "name": name, "display_name": name, "description": "",
        "created_by": "00000000-0000-0000-0000-000000000002", "created_at": "2024-01-01T00:00:00",
        "allow_exec_in_build_requests": false, "restrict_evaluation_fetches": true,
        "extra_allowed_uri_prefixes": [], "hide_build_requests": false, "managed": false,
        "public": false, "public_key": null, "role": null, "running_evaluations": 0,
        "waiting_evaluations": 0, "permissions": []
    })
}

#[tokio::test]
async fn list_projects_reads_every_page() {
    let server = MockServer::start().await;
    for (page, name) in [("1", "first"), ("2", "second")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/projects"))
            .and(query_param("page", page))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
                "items": [project_summary(name)], "total": 2, "page": 1, "per_page": 1
            }))))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.projects().list().await.unwrap();
    let names: Vec<_> = res.items.iter().map(|p| p.name.to_string()).collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(res.total, 2);
}

#[tokio::test]
async fn create_project_sends_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects"))
        .and(body_json(
            json!({ "name": "n", "display_name": "d", "description": "x" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(
            json!({"id": "created-id", "name": "n", "url": "https://ci.example/project/n"}),
        )))
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
            owner_team: None,
        })
        .await
        .unwrap();
    assert_eq!(id.id, "created-id");
    assert_eq!(id.url, "https://ci.example/project/n");
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

#[tokio::test]
async fn subscribe_project_sends_read_only_subscription_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/p/subscribe/c"))
        .and(body_json(json!({ "mode": "ReadOnly" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Subscribed")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .projects()
        .subscribe(
            "p",
            "c",
            connector::api::SubscribeCacheRequest {
                mode: Some(connector::api::CacheSubscriptionMode::ReadOnly),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn get_readiness_decodes_team_without_workers_and_pending_cache_subscription() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/readiness"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "ready": false,
            "workers": {
                "ready": false, "registered": 0, "teams": [], "teams_you_can_grant": [],
                "teams_granted_without_workers": [
                    { "name": "ops", "display_name": "Ops", "workers": 2 }
                ]
            },
            "cache": {
                "ready": false,
                "subscriptions": [{
                    "id": "00000000-0000-0000-0000-000000000003", "name": "main",
                    "display_name": "Main", "mode": "ReadWrite", "status": "pending",
                    "managed": false, "sign_outputs": true, "approvers": ["admin"]
                }]
            },
            "repository": { "status": "unchecked", "tasks": [] }
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let readiness = client.projects().get_project_readiness("p").await.unwrap();
    assert_eq!(
        readiness.workers.teams_granted_without_workers[0].name,
        "ops"
    );
    assert_eq!(readiness.cache.subscriptions[0].approvers, ["admin"]);
}
