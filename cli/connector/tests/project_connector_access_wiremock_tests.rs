// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use connector::api::{
    GrantProjectRequest, PatchProjectGrantRequest, PutProjectSecretRequest, RevokeInvitationRequest,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    json!({ "error": false, "message": m })
}

fn project_secret_summary_json() -> serde_json::Value {
    json!({"name": "TOKEN", "updated_at": "2026-01-01T00:00:00", "updated_by": "alice", "managed": false})
}

#[tokio::test]
async fn list_secret_summaries_returns_names() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/secrets"))
        .and(header("authorization", "Bearer t"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(json!([project_secret_summary_json()]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let secrets = client.projects().list_secret_summaries("p").await.unwrap();
    assert_eq!(secrets[0].name, "TOKEN");
}

#[tokio::test]
async fn get_project_secret_summary_returns_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/secrets/TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(project_secret_summary_json())))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let secret = client
        .projects()
        .get_project_secret_summary("p", "TOKEN")
        .await
        .unwrap();
    assert_eq!(secret.updated_by.as_deref(), Some("alice"));
}

#[tokio::test]
async fn put_project_secret_sends_the_value() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/p/secrets/TOKEN"))
        .and(body_json(json!({"value": "s3cret"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(project_secret_summary_json())))
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
        .put_project_secret(
            "p",
            "TOKEN",
            PutProjectSecretRequest {
                value: "s3cret".into(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_project_secret_returns_true() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/projects/p/secrets/TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(true)))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    assert!(
        client
            .projects()
            .delete_project_secret("p", "TOKEN")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn get_project_sops_recipient_allows_a_missing_age_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/sops-recipient"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({"ssh": "ssh-rsa AAAA"}))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let recipient = client
        .projects()
        .get_project_sops_recipient("p")
        .await
        .unwrap();
    assert_eq!(recipient.age, None);
    assert_eq!(recipient.ssh, "ssh-rsa AAAA");
}

#[tokio::test]
async fn make_project_public_and_private_use_post_and_delete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/p/public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Project is public")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/projects/p/public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Project is private")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client.projects().make_project_public("p").await.unwrap();
    client.projects().make_project_private("p").await.unwrap();
}

#[tokio::test]
async fn list_project_team_grants_returns_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/teams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!([{
            "team": "ops", "display_name": "Ops", "role": "Write",
            "users": true, "workers": false, "pending": true
        }]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let grants = client
        .projects()
        .list_project_team_grants("p")
        .await
        .unwrap();
    assert!(grants[0].pending);
}

#[tokio::test]
async fn grant_team_access_to_project_sends_the_grant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/p/teams"))
        .and(body_json(
            json!({"team": "ops", "role": "View", "users": true, "workers": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Request sent")))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let message = client
        .projects()
        .grant_team_access_to_project(
            "p",
            GrantProjectRequest {
                team: "ops".into(),
                role: Some("View".into()),
                users: Some(true),
                workers: Some(false),
            },
        )
        .await
        .unwrap();
    assert_eq!(message, "Request sent");
}

#[tokio::test]
async fn update_project_team_grant_sends_only_given_fields() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/projects/p/teams/ops"))
        .and(body_json(json!({"workers": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Team grant updated")))
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
        .update_project_team_grant(
            "p",
            "ops",
            PatchProjectGrantRequest {
                role: None,
                users: None,
                workers: Some(true),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_project_team_grant_uses_delete() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/projects/p/teams/ops"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Team grant removed")))
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
        .remove_project_team_grant("p", "ops")
        .await
        .unwrap();
}

#[tokio::test]
async fn list_pending_project_invitations_returns_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/p/invitations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!([{
            "user": "bob", "name": "Bob", "role": "Write",
            "created_at": "2026-01-01T00:00:00", "expires_at": "2026-01-08T00:00:00"
        }]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let invitations = client
        .projects()
        .list_pending_project_invitations("p")
        .await
        .unwrap();
    assert_eq!(invitations[0].user, "bob");
}

#[tokio::test]
async fn revoke_project_invitation_sends_the_user() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/projects/p/invitations"))
        .and(body_json(json!({"user": "bob"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Invitation revoked")))
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
        .revoke_project_invitation("p", RevokeInvitationRequest { user: "bob".into() })
        .await
        .unwrap();
}
