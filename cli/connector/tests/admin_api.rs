// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_admin_workers_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/workers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let workers = client.admin().workers().await.unwrap();
    assert!(workers.is_empty());
}

#[tokio::test]
async fn admin_state_in_nix_format_returns_plain_text_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/state"))
        .and(query_param("format", "nix"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain; charset=utf-8")
                .set_body_string("{ users = { }; }"),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let state = client
        .admin()
        .export_state_configuration_as_text("nix")
        .await
        .unwrap();
    assert_eq!(state, "{ users = { }; }");
}

#[tokio::test]
async fn test_admin_state_json_returns_pretty_printed_envelope_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/state"))
        .and(query_param("format", "json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({"users": {}}))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let state = client
        .admin()
        .export_state_configuration_as_text("json")
        .await
        .unwrap();
    assert_eq!(state, "{\n  \"users\": {}\n}");
}

#[tokio::test]
async fn connector_draining_post_sets_enabled_and_returns_new_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/draining"))
        .and(body_json(serde_json::json!({"enabled": true})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({"draining": true}))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let state = client.admin().set_draining_enabled(true).await.unwrap();
    assert!(state.draining);
}

#[tokio::test]
async fn deep_gc_returns_admin_task_id_and_tasks_list_decodes() {
    let server = MockServer::start().await;
    let task_id = "00000000-0000-0000-0000-00000000000a";
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/maintenance/deep-gc"))
        .respond_with(ResponseTemplate::new(202).set_body_json(ok(
            serde_json::json!({"task_id": task_id, "status": "pending"}),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/tasks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([{
                "id": task_id,
                "kind": "deep_gc",
                "status": "pending",
                "created_at": "2026-11-09T00:00:00Z"
            }]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let started = client.admin().request_deep_gc_round().await.unwrap();
    assert_eq!(started.task_id.0.to_string(), task_id);
    let tasks = client.admin().list_admin_background_tasks().await.unwrap();
    assert_eq!(tasks[0].kind, "deep_gc");
}

#[tokio::test]
async fn admin_webhook_deliveries_endpoint_accepts_limit_and_offset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/webhooks/hook1/deliveries"))
        .and(query_param("limit", "5"))
        .and(query_param("offset", "10"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([{
                "id": "00000000-0000-0000-0000-00000000000b",
                "event": "webhook.ping",
                "success": true,
                "response_status": 200,
                "duration_ms": 8,
                "delivered_at": "2026-11-09T00:00:00Z"
            }]))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let deliveries = client
        .webhooks()
        .list_webhook_delivery_attempts(
            &connector::webhooks::WebhookResource::Instance,
            "hook1",
            Some(5),
            Some(10),
        )
        .await
        .unwrap();
    assert!(deliveries[0].success);
}
