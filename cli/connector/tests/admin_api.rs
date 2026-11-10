// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

fn client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .expect("the client builds")
}

fn registered_worker(draining: bool) -> serde_json::Value {
    serde_json::json!({
        "worker_id": "w1",
        "display_name": "builder",
        "team": {
            "id": "b4361159-0dc0-462c-9dad-411b23394cd8",
            "name": "ops",
            "display_name": "Ops",
            "active": true,
            "managed": false
        },
        "registrations": [],
        "projects": [],
        "connected": false,
        "requested_by_admin": draining
    })
}

#[tokio::test]
async fn list_admin_workers_decodes_registered_workers_with_their_team() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/workers"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok(serde_json::json!([registered_worker(false)]))),
        )
        .mount(&server)
        .await;

    let workers = client(&server).admin().workers().await.unwrap();
    assert_eq!(workers[0].worker_id, "w1");
    assert_eq!(workers[0].team.as_ref().unwrap().name, "ops");
    assert!(!workers[0].connected);
}

#[tokio::test]
async fn worker_drain_patches_the_one_worker() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/admin/workers/w1"))
        .and(body_json(serde_json::json!({"draining": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(registered_worker(true))))
        .expect(1)
        .mount(&server)
        .await;

    let worker = client(&server)
        .admin()
        .set_worker_draining_enabled("w1", true)
        .await
        .unwrap();
    assert!(worker.requested_by_admin);
}

#[tokio::test]
async fn admin_config_decodes_options_with_their_source() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/config"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([
                {
                    "group": "database",
                    "field": "url",
                    "flag": "database-url",
                    "env": "GRADIENT_DATABASE_URL",
                    "secret": true,
                    "source": "env"
                },
                {
                    "group": "eval",
                    "field": "max_keep",
                    "flag": "eval-max-keep",
                    "value": "30",
                    "secret": false,
                    "source": "default"
                }
            ]))),
        )
        .mount(&server)
        .await;

    let options = client(&server)
        .admin()
        .get_admin_config_options_with_values_and_sources()
        .await
        .unwrap();
    assert!(options[0].secret);
    assert_eq!(options[0].value, None);
    assert_eq!(options[1].value.as_deref(), Some("30"));
}

#[tokio::test]
async fn admin_storage_decodes_disk_and_cache_usage() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/storage"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "disk_total_bytes": 1000,
                "disk_free_bytes": 400,
                "caches": [{
                    "id": "640633e7-4189-4eb0-b633-dd440eb2ac3f",
                    "name": "main",
                    "display_name": "Main",
                    "max_storage_gb": 0,
                    "packages": 3,
                    "bytes": 600
                }],
                "projects": [],
                "tasks": []
            }))),
        )
        .mount(&server)
        .await;

    let storage = client(&server)
        .admin()
        .get_admin_storage_disk_usage_and_gc_stats()
        .await
        .unwrap();
    assert_eq!(storage.disk_free_bytes, Some(400));
    assert_eq!(storage.caches[0].bytes, 600);
}

#[tokio::test]
async fn gc_round_accepts_an_empty_202() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/maintenance/gc"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    client(&server).admin().request_gc_round().await.unwrap();
}

#[tokio::test]
async fn gc_round_reports_a_refusal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/maintenance/gc"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    assert!(client(&server).admin().request_gc_round().await.is_err());
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
