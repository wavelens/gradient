/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::integration::IntegrationKind;
use gradient_entity::{github_installation, ids::*, integration, project_user};
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, project, project_member, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, make_token, signed_in, with_auth};
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use gradient_types::{GitHostType, SessionId};
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::Value;
use uuid::Uuid;

fn gitea_inbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000033").unwrap()),
        project: project_id(),
        name: "my-gitea-hook".into(),
        display_name: "My Gitea".into(),
        secret: Some("encrypted-blob".into()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn github_installation_id() -> GithubInstallationId {
    GithubInstallationId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000cc").unwrap())
}

fn github_integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("019e16b2-e958-7652-ad97-67cd7b0fea61").unwrap())
}

fn github_inbound_row() -> integration::Model {
    integration::Model {
        id: github_integration_id(),
        project: project_id(),
        name: "github".into(),
        display_name: "GitHub".into(),
        git_host_type: GitHostType::GitHub,
        github_installation: Some(github_installation_id()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn github_installation_row() -> github_installation::Model {
    github_installation::Model {
        id: github_installation_id(),
        project: project_id(),
        installation_id: 999,
        account_login: Some("acme-corp".into()),
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn gitea_outbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000044").unwrap()),
        project: project_id(),
        name: "my-gitea-reporter".into(),
        display_name: "My Gitea CI".into(),
        kind: IntegrationKind::Outbound,
        endpoint_url: Some("https://gitea.example.com".into()),
        access_token: Some("encrypted-token".into()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![view_role()]])
}

fn with_project_manage(db: MockDatabase) -> MockDatabase {
    with_admin_of(db, project())
}

fn with_admin_of(db: MockDatabase, project: gradient_entity::project::Model) -> MockDatabase {
    db.append_query_results([vec![project]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

fn state_managed_project() -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        managed: true,
        ..project()
    }
}

const SUMMARY_URL: &str = "/api/v1/projects/test-project/integrations/summary";

#[tokio::test]
async fn summary_endpoint_returns_all_kinds() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_member(with_auth(mock_db(), session_id)).append_query_results([vec![
        gitea_inbound_row(),
        github_inbound_row(),
        gitea_outbound_row(),
    ]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(SUMMARY_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let items = body["message"].as_array().expect("array");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["kind"], "inbound");
    assert_eq!(items[0]["git_host_type"], "gitea");
    assert_eq!(items[0]["name"], "my-gitea-hook");
    assert_eq!(items[1]["git_host_type"], "github");
    assert_eq!(items[1]["display_name"], "GitHub");
    assert_eq!(items[2]["kind"], "outbound");
}

#[tokio::test]
async fn summary_endpoint_excludes_credential_state() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![gitea_inbound_row(), gitea_outbound_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(SUMMARY_URL)
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    for item in body["message"].as_array().unwrap() {
        let obj = item.as_object().unwrap();
        for forbidden in [
            "secret",
            "endpoint_url",
            "access_token",
            "has_secret",
            "has_access_token",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "summary leaked `{forbidden}`: {obj:?}"
            );
        }
    }
}

#[tokio::test]
async fn summary_endpoint_rejects_non_member() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_user::Model>::new()]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get(SUMMARY_URL)
        .add_header("authorization", auth)
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn delete_github_integration_removes_pair_and_installation() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let integration_id = github_integration_id();

    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([vec![github_inbound_row()]])
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
        ]);

    let _ = github_installation_row();
    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!(
            "/api/v1/projects/test-project/integrations/{}",
            integration_id
        ))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"], true);
}

#[tokio::test]
async fn github_create_without_app_config_is_rejected() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);

    let db = with_project_manage(with_auth(mock_db(), session_id));

    let server = make_test_server(db.into_connection());
    let res = server
        .put("/api/v1/projects/test-project/integrations")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "my-gh",
            "kind": "outbound",
            "git_host_type": "github",
            "installation_id": 42,
        }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
    assert!(
        body["message"].as_str().unwrap().contains("No GitHub App"),
        "expected 'No GitHub App' in error: {}",
        body["message"]
    );
}

fn delete_url(id: IntegrationId) -> String {
    format!("/api/v1/projects/test-project/integrations/{id}")
}

#[tokio::test]
async fn delete_state_managed_integration_is_forbidden() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let managed = integration::Model {
        managed: true,
        ..gitea_inbound_row()
    };

    let db = with_admin_of(with_auth(mock_db(), session_id), state_managed_project())
        .append_query_results([vec![managed.clone()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&delete_url(managed.id))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_unmanaged_integration_of_state_managed_project_succeeds() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let leftover = gitea_inbound_row();

    let db = with_admin_of(with_auth(mock_db(), session_id), state_managed_project())
        .append_query_results([vec![leftover.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&delete_url(leftover.id))
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], true);
}

#[tokio::test]
async fn members_list_incoming_deliveries_of_an_integration() {
    use gradient_entity::incoming_webhook_delivery::{self, DeliveryResult};

    let session_id = SessionId::now_v7();
    let row = gitea_inbound_row();
    let delivery = incoming_webhook_delivery::Model {
        id: IncomingWebhookDeliveryId::now_v7(),
        integration: row.id,
        event: "push".into(),
        status_code: 401,
        result: DeliveryResult::Refused,
        detail: Some("invalid webhook signature".into()),
        created_at: test_date(),
        ..Default::default()
    };
    let db = with_project_member(with_auth(mock_db(), session_id))
        .append_query_results([vec![row.clone()]])
        .append_query_results([vec![delivery]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("{}/deliveries", delete_url(row.id)))
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let first = &body["message"][0];
    assert_eq!(first["result"], "refused");
    assert_eq!(first["status_code"], 401);
    assert_eq!(first["detail"], "invalid webhook signature");
}

fn server_with_crypt_key(db: MockDatabase) -> axum_test::TestServer {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = path.to_string_lossy().into_owned())
        .server()
}

#[tokio::test]
async fn inbound_create_without_secret_returns_generated_secret_once() {
    let session_id = SessionId::now_v7();
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<integration::Model>::new()])
        .append_query_results([vec![gitea_inbound_row()]]);

    let res = server_with_crypt_key(db)
        .put("/api/v1/projects/test-project/integrations")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({
            "name": "my-gitea-hook",
            "kind": "inbound",
            "git_host_type": "gitea",
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let secret = body["message"]["generated_secret"].as_str().unwrap();
    assert_eq!(secret.len(), 64);
    let webhook_url = body["message"]["webhook_url"].as_str().unwrap();
    assert!(
        webhook_url.ends_with("/api/v1/hooks/gitea/test-project/my-gitea-hook"),
        "{webhook_url}"
    );
    assert!(webhook_url.starts_with("http"), "{webhook_url}");
    assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn connecting_a_git_host_reads_no_tasks_and_returns_its_webhook_secret() {
    let session_id = SessionId::now_v7();
    let inbound = integration::Model {
        endpoint_url: Some("https://gitea.example.com".into()),
        ..gitea_inbound_row()
    };
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<integration::Model>::new()])
        .append_query_results([vec![inbound]]);

    let res = server_with_crypt_key(db)
        .post("/api/v1/projects/test-project/integrations/connect")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({
            "name": "my-gitea-hook",
            "git_host_type": "gitea",
            "endpoint_url": "https://gitea.example.com",
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert!(body["message"]["inbound"].get("connected_tasks").is_none());
    assert_eq!(
        body["message"]["inbound"]["generated_secret"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
}

#[tokio::test]
async fn inbound_create_with_secret_returns_no_generated_secret() {
    let session_id = SessionId::now_v7();
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<integration::Model>::new()])
        .append_query_results([vec![gitea_inbound_row()]]);

    let res = server_with_crypt_key(db)
        .put("/api/v1/projects/test-project/integrations")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({
            "name": "my-gitea-hook",
            "kind": "inbound",
            "git_host_type": "gitea",
            "secret": "chosen-by-the-user",
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert!(body["message"]["generated_secret"].is_null());
}

#[tokio::test]
async fn outbound_create_needs_endpoint_url_and_access_token() {
    for (body, message) in [
        (
            serde_json::json!({"access_token": "tok"}),
            "endpoint_url must be an http or https URL",
        ),
        (
            serde_json::json!({"endpoint_url": "ftp://git.example.com", "access_token": "tok"}),
            "endpoint_url must be an http or https URL",
        ),
        (
            serde_json::json!({"endpoint_url": "https://git.example.com"}),
            "access_token must not be empty",
        ),
    ] {
        let session_id = SessionId::now_v7();
        let db = with_project_manage(with_auth(mock_db(), session_id))
            .append_query_results([Vec::<integration::Model>::new()]);
        let mut request = serde_json::json!({
            "name": "my-gitea-status",
            "kind": "outbound",
            "git_host_type": "gitea",
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());

        let res = server_with_crypt_key(db)
            .put("/api/v1/projects/test-project/integrations")
            .add_header(
                "authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .json(&request)
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["message"], message);
    }
}

#[tokio::test]
async fn outbound_update_rejects_empty_endpoint_url_and_access_token() {
    for (body, message) in [
        (
            serde_json::json!({ "endpoint_url": " " }),
            "endpoint_url must be an http or https URL",
        ),
        (
            serde_json::json!({ "access_token": " " }),
            "access_token must not be empty",
        ),
    ] {
        let session_id = SessionId::now_v7();
        let row = gitea_outbound_row();
        let db = with_project_manage(with_auth(mock_db(), session_id))
            .append_query_results([vec![row.clone()]]);

        let res = server_with_crypt_key(db)
            .patch(&delete_url(row.id))
            .add_header(
                "authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .json(&body)
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["message"], message);
    }
}

#[tokio::test]
async fn clearing_inbound_secret_is_rejected() {
    let session_id = SessionId::now_v7();
    let row = gitea_inbound_row();
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([vec![row.clone()]]);

    let res = server_with_crypt_key(db)
        .patch(&delete_url(row.id))
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&serde_json::json!({ "secret": "" }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("need a webhook secret"),
        "{body}"
    );
}
