// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::{ActionConfig, CreateActionRequest, UpdateActionRequest};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ACTION_ID: &str = "00000000-0000-0000-0000-000000000001";

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

fn action() -> serde_json::Value {
    serde_json::json!({
        "id": ACTION_ID, "name": "deploy-hook", "action_type": "send_web_request",
        "config": {"url": "https://example.com/hook"}, "events": ["evaluation.completed"],
        "active": true, "last_fired_at": null, "created_by": "00000000-0000-0000-0000-000000000002",
        "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00"
    })
}

fn delivery() -> serde_json::Value {
    serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000003", "event": "evaluation.completed", "success": true,
        "response_status": 200, "error_message": null, "duration_ms": 12,
        "delivered_at": "2026-01-01T00:00:00"
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
async fn test_list_task_actions_decodes_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/proj/nightly/actions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok([action()])))
        .expect(1)
        .mount(&server)
        .await;

    let list = client(&server)
        .task_event_actions()
        .list("proj", "nightly")
        .await
        .unwrap();
    assert_eq!(list[0].name, "deploy-hook");
}

#[tokio::test]
async fn creating_send_web_request_action_posts_tagged_config_and_returns_one_time_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/proj/nightly/actions"))
        .and(body_json(serde_json::json!({
            "name": "deploy-hook",
            "config": {"type": "send_web_request", "url": "https://example.com/hook"},
            "events": ["evaluation.completed"],
            "active": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(
            serde_json::json!({"action": action(), "token": "secret"}),
        )))
        .expect(1)
        .mount(&server)
        .await;

    let created = client(&server)
        .task_event_actions()
        .create(
            "proj",
            "nightly",
            &CreateActionRequest {
                name: "deploy-hook".into(),
                config: ActionConfig::SendWebRequest {
                    url: "https://example.com/hook".into(),
                    token: None,
                },
                events: vec!["evaluation.completed".into()],
                active: Some(true),
            },
        )
        .await
        .unwrap();
    assert_eq!(created.token.as_deref(), Some("secret"));
}

#[tokio::test]
async fn test_connector_update_action_sends_only_changed_fields_with_patch() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/v1/tasks/proj/nightly/actions/{ACTION_ID}"
        )))
        .and(body_json(serde_json::json!({"active": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(action())))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .task_event_actions()
        .update(
            "proj",
            "nightly",
            ACTION_ID,
            &UpdateActionRequest {
                active: Some(false),
                config: None,
                events: None,
                name: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_action_test_fire_and_get_one_delivery_use_expected_paths() {
    let server = MockServer::start().await;
    let base = format!("/api/v1/tasks/proj/nightly/actions/{ACTION_ID}");
    Mock::given(method("DELETE"))
        .and(path(base.clone()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({"deleted": true}))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{base}/test")))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("fired")))
        .expect(1)
        .mount(&server)
        .await;
    let mut detail = delivery();
    detail["request_body"] = "{}".into();
    detail["response_body"] = "ok".into();
    Mock::given(method("GET"))
        .and(path(format!(
            "{base}/deliveries/00000000-0000-0000-0000-000000000003"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(detail)))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let actions = client.task_event_actions();
    assert!(
        actions
            .delete("proj", "nightly", ACTION_ID)
            .await
            .unwrap()
            .deleted
    );
    assert_eq!(
        actions.test("proj", "nightly", ACTION_ID).await.unwrap(),
        "fired"
    );
    let detail = actions
        .get_task_action_delivery_with_bodies(
            "proj",
            "nightly",
            ACTION_ID,
            "00000000-0000-0000-0000-000000000003",
        )
        .await
        .unwrap();
    assert_eq!(detail.response_body.as_deref(), Some("ok"));
}

#[tokio::test]
async fn delivery_history_passes_limit_and_offset_as_query_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/tasks/proj/nightly/actions/{ACTION_ID}/deliveries"
        )))
        .and(query_param("limit", "5"))
        .and(query_param("offset", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok([delivery()])))
        .expect(1)
        .mount(&server)
        .await;

    let deliveries = client(&server)
        .task_event_actions()
        .list_delivery_history("proj", "nightly", ACTION_ID, Some(5), Some(10))
        .await
        .unwrap();
    assert!(deliveries[0].success);
}
