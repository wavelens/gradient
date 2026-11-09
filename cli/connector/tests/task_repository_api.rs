// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::{InstallDeployKeyRequest, InstallWebhookRequest, UpdateTaskRepositoryRequest};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INBOUND: &str = "00000000-0000-0000-0000-000000000001";
const OUTBOUND: &str = "00000000-0000-0000-0000-000000000002";
const BASE: &str = "/api/v1/tasks/proj/nightly/repository";

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

fn repository() -> serde_json::Value {
    serde_json::json!({
        "repository": "https://git.example.com/org/repo.git", "uses_ssh": false,
        "integrations": [], "can_edit": true, "can_manage_integrations": true
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
async fn test_task_repository_connection_read_and_replace_uses_get_and_put() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(BASE))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(repository())))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(BASE))
        .and(body_json(serde_json::json!({
            "inbound_integration": INBOUND, "build_on_push": true, "push_branches": ["main"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(repository())))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let api = client.task_repository();
    assert!(!api.get("proj", "nightly").await.unwrap().uses_ssh);
    api.update(
        "proj",
        "nightly",
        &UpdateTaskRepositoryRequest {
            inbound_integration: Some(INBOUND.parse().unwrap()),
            build_on_push: Some(true),
            push_branches: vec!["main".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_install_deploy_key_and_webhook_posts_integration_ids_and_returns_repository_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{BASE}/install-deploy-key")))
        .and(body_json(
            serde_json::json!({"outbound_integration": OUTBOUND}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("org/repo")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{BASE}/install-webhook")))
        .and(body_json(serde_json::json!({
            "inbound_integration": INBOUND, "outbound_integration": OUTBOUND
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("org/repo")))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let api = client.task_repository();
    let path = api
        .add_project_ssh_public_key_as_read_only_deploy_key(
            "proj",
            "nightly",
            &InstallDeployKeyRequest {
                outbound_integration: OUTBOUND.parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(path, "org/repo");
    let path = api
        .create_repository_webhook(
            "proj",
            "nightly",
            &InstallWebhookRequest {
                inbound_integration: INBOUND.parse().unwrap(),
                outbound_integration: OUTBOUND.parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(path, "org/repo");
}
