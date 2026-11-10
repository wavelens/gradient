// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use connector::api::{
    CreateApiKeyRequest, CreateSshKeyRequest, DeleteUserRequest, PatchApiKeyRequest,
};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_user_returns_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "u1", "username": "alice", "name": "Alice", "email": "alice@example.com", "superuser": false,
            "can_create_cache": true
        }))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let user = client.user().get().await.unwrap();
    assert_eq!(user.username, "alice");
}

#[tokio::test]
async fn get_user_with_null_message_for_missing_credentials_returns_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::Value::Null)))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    assert!(matches!(
        client.user().get().await,
        Err(connector::ConnectorError::Unauthorized)
    ));
}

#[tokio::test]
async fn list_keys_returns_vec() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([]))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let keys = client.user().keys().await.unwrap();
    assert!(keys.is_empty());
}

fn connector_client_for_wiremock_with_bearer_token(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .expect("Building the wiremock client")
}

fn one_api_key_info_response_json() -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-000000000001", "name": "ci", "managed": false, "permissions": ["viewProject"],
        "created_at": "2026-01-01T00:00:00", "allowed_ips": []
    })
}

#[tokio::test]
async fn delete_own_account_sends_the_password() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user"))
        .and(body_json(json!({"password": "pw"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("User deleted")))
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .delete_own_account(DeleteUserRequest {
            password: Some("pw".into()),
            confirm_username: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn get_user_settings_returns_login_providers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "username": "alice", "name": "Alice", "email": "a@example.com", "is_oidc": false,
            "has_password": true, "managed": false,
            "login_providers": [{"id": "github", "label": "GitHub", "connected": true, "can_disconnect": true}]
        }))))
        .mount(&server)
        .await;

    let settings = connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .get_user_settings()
        .await
        .unwrap();
    assert!(settings.login_providers[0].connected);
}

#[tokio::test]
async fn create_key_returns_the_token_string() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/keys"))
        .and(body_json(
            json!({"name": "ci", "permissions": ["viewProject"]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("GRADabc")))
        .mount(&server)
        .await;

    let token = connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .create_key(CreateApiKeyRequest {
            name: "ci".into(),
            permissions: vec!["viewProject".into()],
            expires_in_days: None,
            project: None,
            cache: None,
            allowed_ips: None,
        })
        .await
        .unwrap();
    assert_eq!(token, "GRADabc");
}

#[tokio::test]
async fn edit_api_key_patches_by_id() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/user/keys/00000000-0000-0000-0000-000000000001",
        ))
        .and(body_json(json!({"name": "deploy"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(one_api_key_info_response_json())),
        )
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .edit_api_key(
            "00000000-0000-0000-0000-000000000001",
            PatchApiKeyRequest {
                name: Some("deploy".into()),
                permissions: None,
                project: None,
                allowed_ips: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_api_key_by_name_sends_the_name() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/keys"))
        .and(body_json(json!({"name": "ci"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("API-Key deleted")))
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .delete_api_key_by_name("ci")
        .await
        .unwrap();
}

#[tokio::test]
async fn revoke_key_posts_to_the_revoke_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/user/keys/00000000-0000-0000-0000-000000000001/revoke",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("API-Key revoked")))
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .revoke_key("00000000-0000-0000-0000-000000000001")
        .await
        .unwrap();
}

#[tokio::test]
async fn key_permissions_returns_the_catalogue() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/keys/permissions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "available_permissions": [{"id": "viewProject", "mutating": false, "permission_explanation": "View project"}],
            "availableCache": [],
            "server_wide_permissions": [{"id": "createProject", "mutating": true, "permission_explanation": "Create project"}]
        }))))
        .mount(&server)
        .await;

    let catalogue = connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .key_permissions()
        .await
        .unwrap();
    assert_eq!(catalogue.available_permissions[0].id, "viewProject");
    assert_eq!(catalogue.server_wide_permissions[0].id, "createProject");
}

#[tokio::test]
async fn ssh_keys_list_add_and_delete() {
    let server = MockServer::start().await;
    let key = json!({"id": "00000000-0000-0000-0000-000000000002", "name": "laptop", "fingerprint": "SHA256:x", "created_at": "2026-01-01T00:00:00Z"});
    Mock::given(method("GET"))
        .and(path("/api/v1/user/ssh-keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!([key.clone()]))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/ssh-keys"))
        .and(body_json(
            json!({"name": "laptop", "public_key": "ssh-ed25519 AAAA"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(key)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(
            "/api/v1/user/ssh-keys/00000000-0000-0000-0000-000000000002",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Key deleted")))
        .expect(1)
        .mount(&server)
        .await;

    let client = connector_client_for_wiremock_with_bearer_token(&server);
    assert_eq!(
        client.user().get_ssh_keys().await.unwrap()[0].name,
        "laptop"
    );
    client
        .user()
        .add_user_ssh_key(CreateSshKeyRequest {
            name: "laptop".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        })
        .await
        .unwrap();
    client
        .user()
        .delete_user_ssh_key("00000000-0000-0000-0000-000000000002")
        .await
        .unwrap();
}

#[tokio::test]
async fn list_sessions_sends_page_and_per_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/sessions"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!({
            "items": [{
                "id": "[uuid2]", "created_at": "c", "last_used_at": "l", "expires_at": "e",
                "remember_me": false, "current": true
            }],
            "total": 6, "page": 2, "per_page": 5
        }))))
        .mount(&server)
        .await;

    let sessions = connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .list_sessions(Some(2), Some(5))
        .await
        .unwrap();
    assert!(sessions.items[0].current);
}

#[tokio::test]
async fn delete_session_uses_the_session_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/sessions/[uuid2]"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Session revoked")))
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .delete_session("[uuid2]")
        .await
        .unwrap();
}

#[tokio::test]
async fn invitations_list_accept_and_decline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/invites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(json!([{
            "kind": "project", "token": "tok", "scope": "p", "scope_display_name": "P",
            "role": "Write", "invited_by": "alice", "created_at": "2026-01-01T00:00:00",
            "expires_at": "2026-01-08T00:00:00"
        }]))))
        .mount(&server)
        .await;
    for action in ["accept", "decline"] {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/user/invites/{action}")))
            .and(body_json(json!({"token": "tok"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok("done")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = connector_client_for_wiremock_with_bearer_token(&server);
    let invites = client
        .user()
        .list_invitations_addressed_to_me()
        .await
        .unwrap();
    assert_eq!(invites[0].token, "tok");
    client.user().accept_user_invite("tok").await.unwrap();
    client.user().decline_user_invite("tok").await.unwrap();
}

#[tokio::test]
async fn disconnect_user_linked_login_provider_uses_delete() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/identities/github"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Account disconnected")))
        .expect(1)
        .mount(&server)
        .await;

    connector_client_for_wiremock_with_bearer_token(&server)
        .user()
        .disconnect_user_linked_login_provider("github")
        .await
        .unwrap();
}
