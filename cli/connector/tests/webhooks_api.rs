// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::{CreateWebhookRequest, UpdateWebhookRequest};
use connector::webhooks::WebhookResource;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOOK: &str = "00000000-0000-0000-0000-000000000001";

fn project() -> WebhookResource {
    WebhookResource::Project("web".into())
}

fn team() -> WebhookResource {
    WebhookResource::Team("ops".into())
}

fn webhook() -> Value {
    json!({
        "id": HOOK,
        "scope": "project",
        "name": "notify",
        "url": "https://example.com/hook",
        "events": ["build.failed"],
        "active": true,
        "last_delivery": {
            "success": false,
            "delivered_at": "2026-11-01T00:00:00",
            "response_status": 500,
            "error_message": null
        },
        "created_by": "00000000-0000-0000-0000-000000000002",
        "created_at": "2026-11-01T00:00:00",
        "updated_at": "2026-11-01T00:00:00"
    })
}

fn delivery() -> Value {
    json!({
        "id": "0192f0a0-0000-7000-8000-000000000003",
        "event": "webhook.ping",
        "success": true,
        "response_status": 200,
        "duration_ms": 12,
        "delivered_at": "2026-11-01T00:00:00"
    })
}

async fn serve(verb: &str, endpoint: &str, message: Value) -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(method(verb))
        .and(path(format!("/api/v1/{endpoint}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    (server, client)
}

#[tokio::test]
async fn list_project_webhooks_includes_last_delivery() {
    let (_s, client) = serve("GET", "projects/web/webhooks", json!([webhook()])).await;
    let hooks = client.webhooks().list(&project()).await.unwrap();
    let last = hooks[0].last_delivery.as_ref().unwrap();
    assert!(!last.success);
    assert_eq!(last.response_status, Some(500));
}

#[tokio::test]
async fn list_team_webhooks_uses_team_path() {
    let (_s, client) = serve("GET", "teams/ops/webhooks", json!([])).await;
    assert!(client.webhooks().list(&team()).await.unwrap().is_empty());
}

#[tokio::test]
async fn create_returns_webhook_and_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/teams/ops/webhooks"))
        .and(body_json(json!({
            "name": "notify",
            "url": "https://example.com/hook",
            "events": ["build.*"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {"webhook": webhook(), "secret": "whsec"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let created = client
        .webhooks()
        .create(
            &team(),
            CreateWebhookRequest {
                name: "notify".into(),
                url: "https://example.com/hook".into(),
                events: vec!["build.*".into()],
                active: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(created.secret, "whsec");
}

#[tokio::test]
async fn get_reads_one_webhook() {
    let (_s, client) = serve("GET", &format!("projects/web/webhooks/{HOOK}"), webhook()).await;
    let hook = client.webhooks().get(&project(), HOOK).await.unwrap();
    assert_eq!(hook.name, "notify");
}

#[tokio::test]
async fn update_patches_given_fields() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/v1/projects/web/webhooks/{HOOK}")))
        .and(body_json(json!({"events": [], "active": false})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": webhook()})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .webhooks()
        .update(
            &project(),
            HOOK,
            UpdateWebhookRequest {
                events: Some(vec![]),
                active: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_returns_deleted_flag() {
    let (_s, client) = serve(
        "DELETE",
        &format!("projects/web/webhooks/{HOOK}"),
        json!({"deleted": true}),
    )
    .await;
    assert!(
        client
            .webhooks()
            .delete(&project(), HOOK)
            .await
            .unwrap()
            .deleted
    );
}

#[tokio::test]
async fn delivery_attempts_pass_limit_and_offset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/projects/web/webhooks/{HOOK}/deliveries"
        )))
        .and(query_param("limit", "5"))
        .and(query_param("offset", "10"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": false, "message": [delivery()]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let attempts = client
        .webhooks()
        .list_webhook_delivery_attempts(&project(), HOOK, Some(5), Some(10))
        .await
        .unwrap();
    assert_eq!(attempts[0].duration_ms, 12);
}

#[tokio::test]
async fn delivery_attempt_includes_bodies() {
    let mut detail = delivery();
    detail["request_body"] = json!("{\"event\":\"webhook.ping\"}");
    detail["response_body"] = json!("ok");
    let (_s, client) = serve(
        "GET",
        &format!("teams/ops/webhooks/{HOOK}/deliveries/0192f0a0-0000-7000-8000-000000000003"),
        detail,
    )
    .await;
    let attempt = client
        .webhooks()
        .get_webhook_delivery_attempt(&team(), HOOK, "0192f0a0-0000-7000-8000-000000000003")
        .await
        .unwrap();
    assert_eq!(attempt.response_body.as_deref(), Some("ok"));
}

#[tokio::test]
async fn rotate_signing_secret_returns_new_secret() {
    let (_s, client) = serve(
        "POST",
        &format!("projects/web/webhooks/{HOOK}/rotate-secret"),
        json!({"secret": "new"}),
    )
    .await;
    let rotated = client
        .webhooks()
        .rotate_webhook_signing_secret(&project(), HOOK)
        .await
        .unwrap();
    assert_eq!(rotated.secret, "new");
}

#[tokio::test]
async fn send_test_ping_returns_delivery() {
    let (_s, client) = serve(
        "POST",
        &format!("teams/ops/webhooks/{HOOK}/test"),
        delivery(),
    )
    .await;
    let attempt = client
        .webhooks()
        .send_webhook_test_ping(&team(), HOOK)
        .await
        .unwrap();
    assert_eq!(attempt.event, "webhook.ping");
}
