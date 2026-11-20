// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::GitHostConnectionId;
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CONNECTION: &str = "00000000-0000-0000-0000-000000000002";
const ATTEMPT: &str = "00000000-0000-0000-0000-000000000003";
const BASE: &str = "/api/v1/tasks/proj/nightly/repository";

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

fn repository() -> serde_json::Value {
    let off = serde_json::json!({"state": "off", "detail": ""});
    serde_json::json!({
        "repository": "https://git.example.com/org/repo.git", "uses_ssh": false,
        "report_status": false, "status_posts": [], "available_connections": [],
        "chain": {
            "repository": {"state": "ok", "detail": "git.example.com/org/repo"},
            "connection": off, "incoming_events": off, "commit_status": off
        },
        "can_edit": true, "can_manage_integrations": true
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
async fn test_task_repository_connection_read_uses_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(BASE))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(repository())))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let repository = client
        .task_repository()
        .get("proj", "nightly")
        .await
        .unwrap();
    assert!(!repository.uses_ssh);
    assert!(repository.git_host_connection.is_none());
}

#[tokio::test]
async fn test_install_deploy_key_and_webhook_post_without_body_and_return_repository_path() {
    let server = MockServer::start().await;
    for endpoint in ["install-deploy-key", "install-webhook"] {
        Mock::given(method("POST"))
            .and(path(format!("{BASE}/{endpoint}")))
            .and(body_string(""))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok("org/repo")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = client(&server);
    let api = client.task_repository();
    let path = api
        .add_project_ssh_public_key_as_read_only_deploy_key("proj", "nightly")
        .await
        .unwrap();
    assert_eq!(path, "org/repo");
    let path = api
        .create_repository_webhook("proj", "nightly")
        .await
        .unwrap();
    assert_eq!(path, "org/repo");
}

#[tokio::test]
async fn test_set_connection_sends_only_the_given_fields_and_null_to_remove() {
    let server = MockServer::start().await;
    let connection: GitHostConnectionId = CONNECTION.parse().unwrap();
    for (connection, report_status, body) in [
        (
            Some(Some(&connection)),
            Some(true),
            serde_json::json!({"git_host_connection": CONNECTION, "report_status": true}),
        ),
        (
            Some(None),
            None,
            serde_json::json!({"git_host_connection": null}),
        ),
        (
            None,
            Some(false),
            serde_json::json!({"report_status": false}),
        ),
    ] {
        server.reset().await;
        Mock::given(method("PATCH"))
            .and(path(BASE))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok(repository())))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .task_repository()
            .set_task_repository_git_host_connection_and_commit_status_reporting(
                "proj",
                "nightly",
                connection,
                report_status,
            )
            .await
            .unwrap();
        server.verify().await;
    }
}

#[tokio::test]
async fn test_status_test_posts_and_decodes_the_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{BASE}/status-test")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "id": ATTEMPT, "event": "test", "success": true, "response_status": 201,
                "delivered_at": "2026-11-11T00:00:00Z"
            }))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let attempt = client(&server)
        .task_repository()
        .post_pending_test_commit_status_to_last_evaluated_commit("proj", "nightly")
        .await
        .unwrap();
    assert!(attempt.success);
    assert_eq!(attempt.response_status, Some(201));
}
