/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::TestServer;
use gradient_entity::incoming_webhook_delivery::DeliveryResult;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{git_host_connection, incoming_webhook_delivery, project, role, task};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, team_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_test_support::web::bearer;
use gradient_types::ids::*;
use gradient_types::permissions::is_builtin_role;
use gradient_types::{GitHostType, ProjectId};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH: &str =
    "/api/v1/projects/test-project/git-host-connections";

struct RealTestDatabaseHttpServerAndBearerAuthHeader {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    session: SessionId,
    secret_encryption_key_file_path: String,
}

async fn create_test_database_with_project_membership_for_role(
    member_role: role::Model,
) -> RealTestDatabaseHttpServerAndBearerAuthHeader {
    create_test_database_with_project_membership_role_and_server_settings(member_role, |state| {
        state
    })
    .await
}

async fn create_test_database_with_project_membership_role_and_server_settings(
    member_role: role::Model,
    server_settings: impl FnOnce(TestState) -> TestState,
) -> RealTestDatabaseHttpServerAndBearerAuthHeader {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    let session = SessionId::now_v7();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::user())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::session::Entity, _>(
        &conn,
        rows::live_session(session),
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<project::Entity, _>(&conn, rows::project())
        .await
        .unwrap();
    let mut roles = vec![rows::admin_role(), rows::write_role(), rows::view_role()];
    if !is_builtin_role(member_role.id) {
        roles.push(member_role.clone());
    }
    for role in roles {
        gradient_db::rows::insert::<role::Entity, _>(&conn, role)
            .await
            .unwrap();
    }
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        rows::project_member(member_role.id),
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<task::Entity, _>(
        &conn,
        task::Model {
            repository: "git@gitea.example.com:acme/app.git".into(),
            ..rows::task()
        },
    )
    .await
    .unwrap();

    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let secret_encryption_key_file_path = crypt.to_string_lossy().into_owned();
    let crypt_file = secret_encryption_key_file_path.clone();
    let server = server_settings(
        TestState::new()
            .web_db(conn.clone())
            .worker_db(conn)
            .cli(move |cli| cli.secrets.crypt_file = crypt_file),
    )
    .server();
    RealTestDatabaseHttpServerAndBearerAuthHeader {
        db,
        server,
        auth: bearer(session),
        session,
        secret_encryption_key_file_path,
    }
}

fn build_gitea_git_host_connection_for_project_with_event_and_status_flags(
    project: ProjectId,
    base_url: &str,
    reports_status: bool,
) -> git_host_connection::Model {
    git_host_connection::Model {
        id: GitHostConnectionId::now_v7(),
        project: Some(project),
        name: format!("gitea-{}", Uuid::now_v7().simple()),
        display_name: "Gitea".into(),
        git_host_type: GitHostType::Gitea,
        base_url: Some(base_url.into()),
        secret: Some("encrypted".into()),
        access_token: reports_status.then(|| "encrypted".into()),
        receives_events: true,
        reports_status,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

async fn insert_test_git_host_connection(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: git_host_connection::Model,
) -> git_host_connection::Model {
    gradient_db::rows::insert::<git_host_connection::Entity, _>(&s.db.connection(), connection)
        .await
        .unwrap()
}

async fn seed_platform_team_with_test_user_role_as_project_owner(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    team_role: TeamRole,
) {
    let conn = s.db.connection();
    gradient_db::rows::insert::<gradient_entity::team::Entity, _>(&conn, rows::team())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::team_user::Entity, _>(
        &conn,
        rows::team_member(team_role),
    )
    .await
    .unwrap();
    s.db.execute(&format!(
        "UPDATE project SET owner_team = '{}' WHERE id = '{}'",
        team_id(),
        project_id()
    ))
    .await;
}

fn error_message_from_response(res: &axum_test::TestResponse) -> String {
    res.json::<Value>()["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn test_create_project_git_host_connection_without_secret_returns_generated_webhook_secret_and_webhook_url_with_connection_id()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "gitea", "git_host_type": "gitea", "base_url": "https://gitea.example.com" }))
        .await;

    res.assert_status_ok();
    let created = res.json::<Value>()["message"].clone();
    let secret = created["generated_secret"].as_str().unwrap();
    assert_eq!(secret.len(), 64, "{created}");
    assert!(secret.chars().all(|c| c.is_ascii_hexdigit()), "{created}");
    let id = created["id"].as_str().unwrap();
    assert!(
        created["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/api/v1/hooks/{id}")),
        "{created}"
    );
    assert_eq!(created["has_secret"], true);
    assert_eq!(
        created["owner"],
        json!({ "type": "project", "name": "test-project" })
    );
    let stored = s.db.rows::<git_host_connection::Entity>().await;
    assert_eq!(stored.len(), 1);
    assert_ne!(stored[0].secret.as_deref(), Some(secret));
    s.db.finish().await;
}

#[tokio::test]
async fn project_git_host_connections_list_shows_no_credentials() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;

    let res = s
        .server
        .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_ok();
    let list = res.json::<Value>()["message"].as_array().unwrap().clone();
    assert_eq!(list.len(), 1);
    let entry = list[0].as_object().unwrap();
    for key in ["secret", "access_token", "has_secret", "has_access_token"] {
        assert!(!entry.contains_key(key), "{key} in {entry:?}");
    }
    assert_eq!(entry["can_manage"], true);
    s.db.finish().await;
}

#[tokio::test]
async fn team_admin_created_team_git_host_connection_is_available_to_team_project_and_task_can_select_it()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;

    let created = s
        .server
        .put("/api/v1/teams/platform/git-host-connections")
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "team-gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
        }))
        .await;
    created.assert_status_ok();
    let id = created.json::<Value>()["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gradient_db::git_host_connection_repository_grants::insert(
        &s.db.connection(),
        gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
            connection: id.parse().unwrap(),
            project: project_id(),
            repository_owner: "acme",
            repository_name: Some("app"),
            source:
                gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource::Admin,
            created_by: Some(user_id()),
        },
    )
    .await
    .unwrap();
    let available = s
        .server
        .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .await;
    let selected = s
        .server
        .patch("/api/v1/tasks/test-project/test-task/repository")
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": id, "report_status": false }))
        .await;

    available.assert_status_ok();
    let list = available.json::<Value>()["message"]
        .as_array()
        .unwrap()
        .clone();
    let team_connection = list.iter().find(|c| c["id"] == id.as_str()).unwrap();
    assert_eq!(
        team_connection["owner"],
        json!({ "type": "team", "name": "platform" })
    );
    assert_eq!(team_connection["can_manage"], false);
    selected.assert_status_ok();
    let task =
        s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
            .await
            .unwrap();
    assert_eq!(
        task.git_host_connection.map(|c| c.to_string()),
        Some(id.clone())
    );
    s.db.finish().await;
}

#[tokio::test]
async fn non_admin_team_member_cannot_create_team_git_host_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Member).await;

    let res = s
        .server
        .put("/api/v1/teams/platform/git-host-connections")
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "team-gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
        }))
        .await;

    res.assert_status_forbidden();
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn project_cannot_create_github_git_host_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "github", "git_host_type": "github", "installation_id": 9999 }))
        .await;

    res.assert_status_bad_request();
    assert!(
        error_message_from_response(&res).contains("GitHub App"),
        "{}",
        res.text()
    );
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn git_host_connection_for_commit_status_requires_base_url_and_access_token() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "status",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
            "receives_events": false,
            "reports_status": true,
        }))
        .await;

    res.assert_status_bad_request();
    assert_eq!(
        error_message_from_response(&res),
        "The Git host URL and access token are required to report commit status."
    );
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn deleting_git_host_connection_managed_by_declarative_state_is_forbidden() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let managed = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            managed: true,
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;

    let listed = s
        .server
        .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .await;
    listed.assert_status_ok();
    assert_eq!(listed.json::<Value>()["message"][0]["managed"], true);

    let res = s
        .server
        .delete(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            managed.id
        ))
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_forbidden();
    assert_eq!(s.db.rows::<git_host_connection::Entity>().await.len(), 1);
    s.db.finish().await;
}

#[tokio::test]
async fn removing_webhook_secret_of_connection_receiving_events_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "secret": "" }))
        .await;

    res.assert_status_bad_request();
    assert!(
        error_message_from_response(&res).contains("webhook secret"),
        "{}",
        res.text()
    );
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    assert_eq!(stored.secret.as_deref(), Some("encrypted"));
    s.db.finish().await;
}

async fn test_project_connection(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
) -> axum_test::TestResponse {
    s.server
        .post(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{connection}/test"
        ))
        .add_header("authorization", &s.auth)
        .await
}

#[tokio::test]
async fn testing_a_connection_reports_why_sign_in_is_impossible() {
    let s = create_test_database_with_project_membership_role_and_server_settings(
        rows::admin_role(),
        |state| state.cli(|cli| cli.outbound.allowed_non_public_network_targets.clear()),
    )
    .await;
    let loopback = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "http://127.0.0.1:9",
            true,
        ),
    )
    .await;
    let tokenless = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    let res = test_project_connection(&s, loopback.id).await;
    res.assert_status_ok();
    let result = res.json::<Value>()["message"].clone();
    assert_eq!(result["success"], false);
    assert_eq!(result["account"], Value::Null);
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .starts_with("Gradient does not call http://127.0.0.1:9"),
        "{result}"
    );

    let result = test_project_connection(&s, tokenless.id)
        .await
        .json::<Value>()["message"]
        .clone();
    assert_eq!(result["success"], false);
    assert_eq!(result["error"], "The connection has no access token.");
    s.db.finish().await;
}

#[tokio::test]
async fn testing_a_connection_needs_manage_integrations() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    test_project_connection(&s, connection.id)
        .await
        .assert_status_forbidden();
    s.db.finish().await;
}

#[tokio::test]
async fn saving_events_only_gitea_connection_with_missing_or_blank_base_url_is_refused_with_422() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    for base_url in [None, Some(""), Some("   ")] {
        let res = s
            .server
            .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
            .add_header("authorization", &s.auth)
            .json(&json!({
                "name": "gitea",
                "git_host_type": "gitea",
                "base_url": base_url,
                "receives_events": true,
                "reports_status": false,
            }))
            .await;

        res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            error_message_from_response(&res).contains("base_url"),
            "{base_url:?}: {}",
            res.text()
        );
    }
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn saving_git_host_connection_with_credentials_query_or_fragment_in_base_url_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    for base_url in [
        "https://user:pass@gitea.example.com",
        "https://gitea.example.com?token=x",
        "https://gitea.example.com#part",
        "ftp://gitea.example.com",
    ] {
        let res = s
            .server
            .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
            .add_header("authorization", &s.auth)
            .json(&json!({ "name": "gitea", "git_host_type": "gitea", "base_url": base_url }))
            .await;

        assert!(res.status_code().is_client_error(), "{base_url}");
    }
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn updating_a_gitea_connection_to_remove_its_base_url_is_refused_with_422() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "" }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    assert_eq!(
        stored.base_url.as_deref(),
        Some("https://gitea.example.com")
    );
    s.db.finish().await;
}

#[tokio::test]
async fn create_gitlab_git_host_connection_without_base_url_defaults_to_gitlab_com() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "gitlab", "git_host_type": "gitlab" }))
        .await;

    res.assert_status_ok();
    let created = res.json::<Value>()["message"].clone();
    assert_eq!(created["base_url"], "https://gitlab.com", "{created}");
    let stored = s.db.rows::<git_host_connection::Entity>().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].base_url.as_deref(), Some("https://gitlab.com"));
    s.db.finish().await;
}

fn build_project_gitlab_git_host_connection_at_base_url_with_oauth_access_token_refresh_token_and_expiry(
    base_url: &str,
) -> git_host_connection::Model {
    connection_with_oauth_sign_in_tokens(git_host_connection::Model {
        git_host_type: GitHostType::GitLab,
        ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            base_url,
            false,
        )
    })
}

#[tokio::test]
async fn patch_gitlab_connection_with_empty_base_url_moves_to_gitlab_com_and_clears_tokens() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_project_gitlab_git_host_connection_at_base_url_with_oauth_access_token_refresh_token_and_expiry("https://gitlab.example.com"),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "" }))
        .await;

    assert_eq!(
        res.json::<Value>()["message"]["base_url"],
        "https://gitlab.com",
        "{}",
        res.text()
    );
    assert_git_host_connection_has_no_tokens_or_expiry_after_patch(&s, connection.id, res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn patch_gitlab_connection_empty_base_url_keeps_gitlab_com_url_and_access_token() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_project_gitlab_git_host_connection_at_base_url_with_oauth_access_token_refresh_token_and_expiry("https://gitlab.com"),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "" }))
        .await;

    res.assert_status_ok();
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    assert_eq!(stored.base_url.as_deref(), Some("https://gitlab.com"));
    assert_eq!(stored.access_token, connection.access_token);
    assert_eq!(
        stored.encrypted_refresh_token,
        connection.encrypted_refresh_token
    );
    assert_eq!(stored.secret, connection.secret);
    s.db.finish().await;
}

#[tokio::test]
async fn project_members_can_list_incoming_webhook_deliveries_of_project_git_host_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;
    gradient_db::rows::insert::<incoming_webhook_delivery::Entity, _>(
        &s.db.connection(),
        incoming_webhook_delivery::Model {
            id: IncomingWebhookDeliveryId::now_v7(),
            git_host_connection: connection.id,
            event: "push".into(),
            status_code: 200,
            result: DeliveryResult::Queued,
            repository: Some("https://gitea.example.com/acme/app".into()),
            tasks_scanned: 1,
            queued: 1,
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let res = s
        .server
        .get(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/deliveries",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_ok();
    let deliveries = res.json::<Value>()["message"].as_array().unwrap().clone();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["event"], "push");
    assert_eq!(deliveries[0]["result"], "queued");
    assert_eq!(deliveries[0]["queued"], 1);
    s.db.finish().await;
}

#[tokio::test]
async fn creating_updating_and_deleting_a_project_git_host_connection_writes_audit_events() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let created = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "gitea", "git_host_type": "gitea", "base_url": "https://gitea.example.com" }))
        .await;
    created.assert_status_ok();
    let path = format!(
        "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
        created.json::<Value>()["message"]["id"].as_str().unwrap()
    );
    s.server
        .patch(&path)
        .add_header("authorization", &s.auth)
        .json(&json!({ "display_name": "Gitea" }))
        .await
        .assert_status_ok();
    s.server
        .delete(&path)
        .add_header("authorization", &s.auth)
        .await
        .assert_status_ok();

    let mut events: Vec<(chrono::NaiveDateTime, String)> =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| row.event.starts_with("git_host_connection."))
            .map(|row| (row.created_at, row.event))
            .collect();
    events.sort();
    let events: Vec<String> = events.into_iter().map(|(_, event)| event).collect();
    assert_eq!(
        events,
        [
            "git_host_connection.create",
            "git_host_connection.update",
            "git_host_connection.delete"
        ]
    );
    s.db.finish().await;
}

#[tokio::test]
async fn replacing_the_access_token_ends_the_automatic_token_refresh() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            encrypted_refresh_token: Some("encrypted".into()),
            access_token_expires_at: Some(test_date()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                true,
            )
        },
    )
    .await;
    let path = format!("{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}", connection.id);

    let before = s
        .server
        .get(&path)
        .add_header("authorization", &s.auth)
        .await;
    assert_eq!(
        before.json::<Value>()["message"]["access_token_refreshes_automatically"],
        true
    );
    let after = s
        .server
        .patch(&path)
        .add_header("authorization", &s.auth)
        .json(&json!({ "access_token": "new-token" }))
        .await;

    after.assert_status_ok();
    assert_eq!(
        after.json::<Value>()["message"]["access_token_refreshes_automatically"],
        false
    );
    let stored = s.db.rows::<git_host_connection::Entity>().await;
    assert_eq!(stored[0].encrypted_refresh_token, None);
    assert_eq!(stored[0].access_token_expires_at, None);
    s.db.finish().await;
}

fn connection_with_oauth_sign_in_tokens(
    connection: git_host_connection::Model,
) -> git_host_connection::Model {
    git_host_connection::Model {
        access_token: Some("encrypted".into()),
        encrypted_refresh_token: Some("encrypted".into()),
        access_token_expires_at: Some(test_date()),
        reports_status: false,
        ..connection
    }
}

async fn assert_git_host_connection_has_no_tokens_or_expiry_after_patch(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
    res: axum_test::TestResponse,
) {
    res.assert_status_ok();
    let summary = res.json::<Value>()["message"].clone();
    assert_eq!(summary["has_access_token"], false, "{summary}");
    assert_eq!(
        summary["access_token_refreshes_automatically"], false,
        "{summary}"
    );
    assert_eq!(summary["access_token_expires_at"], Value::Null, "{summary}");
    let stored =
        s.db.find::<git_host_connection::Entity>(connection)
            .await
            .unwrap();
    assert_eq!(stored.access_token, None);
    assert_eq!(stored.encrypted_refresh_token, None);
    assert_eq!(stored.access_token_expires_at, None);
    assert_stored_webhook_secret_replaced_and_decrypts_to_generated_secret(s, &stored, &summary);
}

fn assert_stored_webhook_secret_replaced_and_decrypts_to_generated_secret(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    stored: &git_host_connection::Model,
    summary: &Value,
) {
    let generated = summary["generated_secret"]
        .as_str()
        .unwrap_or_else(|| panic!("{summary}"));
    let secret = gradient_ci::actions::decrypt_secret_with_file(
        &s.secret_encryption_key_file_path,
        stored.secret.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(secret.expose(), generated);
}

#[tokio::test]
async fn patch_project_git_host_connection_created_by_oauth_change_base_url_removes_stored_tokens()
{
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            ),
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "https://other.example.com" }))
        .await;

    assert_git_host_connection_has_no_tokens_or_expiry_after_patch(&s, connection.id, res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn patch_team_git_host_connection_base_url_without_access_token_removes_stored_token() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            reports_status: false,
            ..team_connection_for_task_repository_host()
        },
    )
    .await;
    assert!(connection.access_token.is_some());

    let res = s
        .server
        .patch(&format!(
            "/api/v1/teams/platform/git-host-connections/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "https://other.example.com" }))
        .await;

    assert_git_host_connection_has_no_tokens_or_expiry_after_patch(&s, connection.id, res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn admin_patch_git_host_type_without_access_token_removes_stored_tokens() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            make_instance_gitea_connection_for_task_repository_host(),
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "/api/v1/admin/git-host-connections/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_type": "forgejo" }))
        .await;

    assert_git_host_connection_has_no_tokens_or_expiry_after_patch(&s, connection.id, res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn patch_without_token_keeps_stored_tokens() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            ),
        ),
    )
    .await;

    for base_url in [
        "https://gitea.example.com/",
        "https://GITEA.example.com:443/forgejo",
    ] {
        let res = s
            .server
            .patch(&format!(
                "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
                connection.id
            ))
            .add_header("authorization", &s.auth)
            .json(&json!({ "base_url": base_url, "git_host_type": "gitea" }))
            .await;

        res.assert_status_ok();
        let summary = res.json::<Value>()["message"].clone();
        assert_eq!(summary["has_access_token"], true, "{base_url}: {summary}");
        assert_eq!(summary["generated_secret"], Value::Null, "{base_url}");
        let stored =
            s.db.find::<git_host_connection::Entity>(connection.id)
                .await
                .unwrap();
        assert_eq!(stored.access_token, connection.access_token, "{base_url}");
        assert_eq!(
            stored.encrypted_refresh_token, connection.encrypted_refresh_token,
            "{base_url}"
        );
        assert_eq!(
            stored.access_token_expires_at, connection.access_token_expires_at,
            "{base_url}"
        );
        assert_eq!(stored.secret, connection.secret, "{base_url}");
    }
    s.db.finish().await;
}

#[tokio::test]
async fn patch_display_name_waits_for_row_lock_and_keeps_concurrently_rotated_refresh_token() {
    use sea_orm::{EntityTrait, QuerySelect, TransactionTrait};

    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            ),
        ),
    )
    .await;
    let concurrent_oauth_token_refresh_transaction = s.db.connection().begin().await.unwrap();
    let git_host_connection_row_read_under_exclusive_lock =
        git_host_connection::Entity::find_by_id(connection.id)
            .lock_exclusive()
            .one(&concurrent_oauth_token_refresh_transaction)
            .await
            .unwrap()
            .unwrap();

    let mut pinned_patch_display_name_request_future = std::pin::pin!(
        s.server
            .patch(&format!(
                "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
                connection.id
            ))
            .add_header("authorization", &s.auth)
            .json(&json!({ "display_name": "Renamed" }))
            .into_future()
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            &mut pinned_patch_display_name_request_future,
        )
        .await
        .is_err()
    );
    gradient_db::git_host_connections::write_all_columns_of_git_host_connection(
        &concurrent_oauth_token_refresh_transaction,
        git_host_connection::Model {
            encrypted_refresh_token: Some("rotated".into()),
            ..git_host_connection_row_read_under_exclusive_lock
        },
    )
    .await
    .unwrap();
    concurrent_oauth_token_refresh_transaction
        .commit()
        .await
        .unwrap();

    pinned_patch_display_name_request_future
        .await
        .assert_status_ok();
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    assert_eq!(stored.display_name, "Renamed");
    assert_eq!(stored.encrypted_refresh_token.as_deref(), Some("rotated"));
    s.db.finish().await;
}

#[tokio::test]
async fn patch_base_url_from_https_to_http_without_token_clears_stored_secrets() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            ),
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "http://gitea.example.com" }))
        .await;

    assert_git_host_connection_has_no_tokens_or_expiry_after_patch(&s, connection.id, res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn patch_base_url_to_another_host_with_new_access_token_replaces_access_token_and_clears_refresh_token_and_expiry()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        connection_with_oauth_sign_in_tokens(
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            ),
        ),
    )
    .await;

    let res = s
        .server
        .patch(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .json(&json!({ "base_url": "https://other.example.com", "access_token": "new-token" }))
        .await;

    res.assert_status_ok();
    let summary = res.json::<Value>()["message"].clone();
    assert_eq!(summary["has_access_token"], true);
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    assert_stored_webhook_secret_replaced_and_decrypts_to_generated_secret(&s, &stored, &summary);
    let token = gradient_ci::actions::decrypt_secret_with_file(
        &s.secret_encryption_key_file_path,
        stored.access_token.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(token.expose(), "new-token");
    assert_eq!(stored.encrypted_refresh_token, None);
    assert_eq!(stored.access_token_expires_at, None);
    s.db.finish().await;
}

async fn insert_task_repository_grant_allowing_project_to_use_git_host_connection(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
    project: ProjectId,
) {
    gradient_db::git_host_connection_repository_grants::insert(
        &s.db.connection(),
        gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
            connection,
            project,
            repository_owner: "acme",
            repository_name: Some("app"),
            source:
                gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource::Admin,
            created_by: Some(user_id()),
        },
    )
    .await
    .unwrap();
}

async fn insert_project_named_target_project_and_add_test_user_as_admin_member(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> project::Model {
    let conn = s.db.connection();
    let target = project::Model {
        id: ProjectId::now_v7(),
        name: "target-project".into(),
        ..rows::project()
    };
    gradient_db::rows::insert::<project::Entity, _>(&conn, target.clone())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        gradient_entity::project_user::Model {
            project: target.id,
            ..rows::project_member(rows::admin_role().id)
        },
    )
    .await
    .unwrap();
    target
}

async fn send_request_to_move_test_task_to_target_project(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> axum_test::TestResponse {
    s.server
        .post("/api/v1/tasks/test-project/test-task/transfer")
        .add_header("authorization", &s.auth)
        .json(&json!({ "project": "target-project" }))
        .await
}

async fn read_project_id_for_test_task(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> ProjectId {
    s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
        .await
        .unwrap()
        .project
}

fn make_instance_gitea_connection_for_task_repository_host() -> git_host_connection::Model {
    git_host_connection::Model {
        project: None,
        ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        )
    }
}

#[tokio::test]
async fn moving_task_with_instance_connection_requires_repository_grant_in_target_project() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let target = insert_project_named_target_project_and_add_test_user_as_admin_member(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        instance.id,
        project_id(),
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}' WHERE id = '{}'",
        instance.id,
        gradient_fixtures::ids::task_id()
    ))
    .await;

    send_request_to_move_test_task_to_target_project(&s)
        .await
        .assert_status_conflict();
    assert_eq!(read_project_id_for_test_task(&s).await, project_id());

    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        instance.id,
        target.id,
    )
    .await;
    send_request_to_move_test_task_to_target_project(&s)
        .await
        .assert_status_ok();
    assert_eq!(read_project_id_for_test_task(&s).await, target.id);
    s.db.finish().await;
}

#[tokio::test]
async fn move_task_with_instance_connection_open_pr_action_and_no_target_repository_grant_is_refused()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let target = insert_project_named_target_project_and_add_test_user_as_admin_member(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    gradient_db::rows::insert::<gradient_entity::task_action::Entity, _>(
        &s.db.connection(),
        gradient_entity::task_action::Model {
            id: TaskActionId::now_v7(),
            task: gradient_fixtures::ids::task_id(),
            name: "update-inputs".into(),
            action_type: gradient_types::ActionType::OpenPr,
            config: json!({ "type": "open_pr", "git_host_connection_id": instance.id }),
            events: json!([]),
            active: true,
            created_by: user_id(),
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    send_request_to_move_test_task_to_target_project(&s)
        .await
        .assert_status_conflict();
    assert_eq!(read_project_id_for_test_task(&s).await, project_id());

    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        instance.id,
        target.id,
    )
    .await;
    send_request_to_move_test_task_to_target_project(&s)
        .await
        .assert_status_ok();
    assert_eq!(read_project_id_for_test_task(&s).await, target.id);
    s.db.finish().await;
}

#[tokio::test]
async fn moving_a_task_needs_a_git_host_connection_the_target_project_can_use() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let conn = s.db.connection();
    let target = project::Model {
        id: ProjectId::now_v7(),
        name: "target-project".into(),
        ..rows::project()
    };
    gradient_db::rows::insert::<project::Entity, _>(&conn, target.clone())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        gradient_entity::project_user::Model {
            project: target.id,
            ..rows::project_member(rows::admin_role().id)
        },
    )
    .await
    .unwrap();
    let project_connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;
    let instance_connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            project: None,
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;
    let use_connection = |id: GitHostConnectionId| {
        format!(
            "UPDATE task SET git_host_connection = '{id}' WHERE id = '{}'",
            gradient_fixtures::ids::task_id()
        )
    };
    let transfer = || {
        s.server
            .post("/api/v1/tasks/test-project/test-task/transfer")
            .add_header("authorization", &s.auth)
            .json(&json!({ "project": "target-project" }))
    };

    s.db.execute(&use_connection(project_connection.id)).await;
    transfer().await.assert_status_conflict();

    s.db.execute(&use_connection(instance_connection.id)).await;
    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        instance_connection.id,
        target.id,
    )
    .await;
    transfer().await.assert_status_ok();
    s.db.finish().await;
}

#[tokio::test]
async fn view_members_see_connection_health_and_repository_fit_in_the_project_list() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            refused_signature_count: 3,
            last_refused_signature_at: Some(test_date()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                true,
            )
        },
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}'",
        connection.id
    ))
    .await;
    gradient_db::rows::insert::<incoming_webhook_delivery::Entity, _>(
        &s.db.connection(),
        incoming_webhook_delivery::Model {
            id: IncomingWebhookDeliveryId::now_v7(),
            git_host_connection: connection.id,
            event: "push".into(),
            status_code: 200,
            result: DeliveryResult::Queued,
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let list = |repository: &'static str| {
        s.server
            .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
            .add_query_param("repository", repository)
            .add_header("authorization", &s.auth)
    };

    let res = list("https://gitea.example.com/acme/app").await;
    res.assert_status_ok();
    let entry = res.json::<Value>()["message"][0].clone();
    assert_eq!(entry["can_manage"], false, "{entry}");
    assert!(
        entry["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/api/v1/hooks/{}", connection.id)),
        "{entry}"
    );
    assert_eq!(entry["task_count"], 1, "{entry}");
    assert_eq!(entry["refused_signature_count"], 3, "{entry}");
    assert_eq!(entry["last_delivery"]["result"], "queued", "{entry}");
    assert_eq!(
        entry["repository_fit"],
        json!({ "fits": true, "can_be_selected": true, "reason": null })
    );

    let res = list("https://github.com/acme/app").await;
    let fit = res.json::<Value>()["message"][0]["repository_fit"].clone();
    assert_eq!(fit["fits"], false, "{fit}");
    assert!(
        fit["reason"].as_str().unwrap().contains("github.com"),
        "{fit}"
    );
    s.db.finish().await;
}

#[tokio::test]
async fn a_github_app_installation_names_its_installation_id_in_the_list() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            id: GitHostConnectionId::now_v7(),
            name: "github-acme".into(),
            display_name: "GitHub".into(),
            git_host_type: GitHostType::GitHub,
            receives_events: true,
            reports_status: true,
            github_installation_id: Some(4242),
            account_login: Some("acme".into()),
            created_by: user_id(),
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await;

    let res = s
        .server
        .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_ok();
    let list = res.json::<Value>()["message"].clone();
    let installation = list
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "github-acme")
        .unwrap_or_else(|| panic!("{list}"));
    assert_eq!(installation["github_installation_id"], 4242);
    s.db.finish().await;
}

#[tokio::test]
async fn creating_a_project_connection_links_fitting_tasks_without_a_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
            "access_token": "token",
            "reports_status": true,
        }))
        .await;

    res.assert_status_ok();
    let created = res.json::<Value>()["message"].clone();
    assert_eq!(created["task_count"], 1, "{created}");
    let task =
        s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
            .await
            .unwrap();
    assert_eq!(
        task.git_host_connection.map(|id| id.to_string()),
        created["id"].as_str().map(str::to_owned)
    );
    assert!(task.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn creating_a_project_connection_leaves_a_task_of_declarative_state_without_a_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    s.db.execute("UPDATE task SET managed = true").await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
            "access_token": "token",
            "reports_status": true,
        }))
        .await;

    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"]["task_count"], 0);
    let task =
        s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
            .await
            .unwrap();
    assert_eq!(task.git_host_connection, None);
    assert!(!task.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn a_connection_without_a_known_host_links_no_task() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "forgejo", "git_host_type": "forgejo" }))
        .await;

    res.assert_status_unprocessable_entity();
    assert!(
        error_message_from_response(&res).contains("base_url"),
        "{}",
        res.text()
    );
    assert!(s.db.rows::<git_host_connection::Entity>().await.is_empty());
    let task =
        s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
            .await
            .unwrap();
    assert_eq!(task.git_host_connection, None);
    s.db.finish().await;
}

#[tokio::test]
async fn the_task_list_of_a_connection_hides_tasks_of_projects_the_caller_cannot_see() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            project: None,
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;
    let hidden_project = ProjectId::now_v7();
    gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        project::Model {
            id: hidden_project,
            name: "hidden-project".into(),
            public: false,
            ..rows::project()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<task::Entity, _>(
        &s.db.connection(),
        task::Model {
            id: TaskId::now_v7(),
            project: hidden_project,
            ..rows::task()
        },
    )
    .await
    .unwrap();
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}'",
        connection.id
    ))
    .await;

    let res = s
        .server
        .get(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/tasks",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_ok();
    let tasks = res.json::<Value>()["message"].as_array().unwrap().clone();
    let summaries = s
        .server
        .get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .await
        .json::<Value>()["message"]
        .clone();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0]["project"], "test-project");
    assert_eq!(tasks[0]["name"], "test-task");
    assert_eq!(summaries[0]["task_count"], 1, "{summaries}");
    s.db.finish().await;
}

#[tokio::test]
async fn an_api_key_pinned_to_a_project_reads_only_that_project_on_a_shared_connection() {
    use sha2::{Digest, Sha256};
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let conn = s.db.connection();
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            project: None,
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;
    let other_project = ProjectId::now_v7();
    gradient_db::rows::insert::<project::Entity, _>(
        &conn,
        project::Model {
            id: other_project,
            name: "other-project".into(),
            ..rows::project()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        gradient_entity::project_user::Model {
            id: ProjectUserId::now_v7(),
            project: other_project,
            ..rows::project_member(rows::admin_role().id)
        },
    )
    .await
    .unwrap();
    let other_task = TaskId::now_v7();
    gradient_db::rows::insert::<task::Entity, _>(
        &conn,
        task::Model {
            id: other_task,
            project: other_project,
            name: "other-task".into(),
            repository: "https://gitea.example.com/acme/other".into(),
            ..rows::task()
        },
    )
    .await
    .unwrap();
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}'",
        connection.id
    ))
    .await;
    for (offset, repository) in [(0, "acme/app"), (60, "acme/other")] {
        gradient_db::rows::insert::<incoming_webhook_delivery::Entity, _>(
            &conn,
            incoming_webhook_delivery::Model {
                id: IncomingWebhookDeliveryId::now_v7(),
                git_host_connection: connection.id,
                event: "push".into(),
                status_code: 200,
                result: DeliveryResult::Queued,
                repository: Some(format!("https://gitea.example.com/{repository}")),
                created_at: test_date() + chrono::Duration::seconds(offset),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    gradient_db::rows::insert::<gradient_entity::task_commit_status_post_attempt::Entity, _>(
        &conn,
        gradient_entity::task_commit_status_post_attempt::Model {
            id: TaskCommitStatusPostAttemptId::now_v7(),
            task: other_task,
            git_host_connection: Some(connection.id),
            event: "success".into(),
            success: true,
            delivered_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let raw = "p".repeat(64);
    gradient_db::rows::insert::<gradient_entity::api::Entity, _>(
        &conn,
        gradient_entity::api::Model {
            id: ApiId::now_v7(),
            owned_by: user_id(),
            name: "pinned".into(),
            key: Sha256::digest(raw.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            last_used_at: test_date(),
            created_at: test_date(),
            permission: gradient_types::permissions::admin_mask(),
            project: Some(project_id()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let get = |path: String, auth: String| {
        let request = s.server.get(&path).add_header("authorization", auth);
        async move {
            let res = request.await;
            res.assert_status_ok();
            res.json::<Value>()["message"].clone()
        }
    };
    let key = format!("Bearer GRAD{raw}");
    let tasks_path = format!(
        "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/tasks",
        connection.id
    );
    let deliveries_path = format!(
        "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/deliveries",
        connection.id
    );

    let summary = get(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH.into(), key.clone()).await[0].clone();
    let tasks = get(tasks_path.clone(), key.clone()).await;
    let deliveries = get(deliveries_path.clone(), key).await;
    let session_summary = get(
        TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH.into(),
        s.auth.clone(),
    )
    .await[0]
        .clone();
    let session_tasks = get(tasks_path, s.auth.clone()).await;
    let session_deliveries = get(deliveries_path, s.auth.clone()).await;

    assert_eq!(summary["task_count"], 1, "{summary}");
    assert_eq!(summary["last_status_post"], Value::Null, "{summary}");
    assert_eq!(
        summary["last_delivery"]["repository"], "https://gitea.example.com/acme/app",
        "{summary}"
    );
    assert_eq!(tasks.as_array().unwrap().len(), 1, "{tasks}");
    assert_eq!(tasks[0]["name"], "test-task");
    assert_eq!(deliveries.as_array().unwrap().len(), 1, "{deliveries}");
    assert_eq!(session_summary["task_count"], 2, "{session_summary}");
    assert_eq!(
        session_summary["last_delivery"]["repository"], "https://gitea.example.com/acme/other",
        "{session_summary}"
    );
    assert_ne!(session_summary["last_status_post"], Value::Null);
    assert_eq!(session_tasks.as_array().unwrap().len(), 2);
    assert_eq!(session_deliveries.as_array().unwrap().len(), 2);
    s.db.finish().await;
}

#[tokio::test]
async fn project_members_can_list_deliveries_of_a_team_connection_the_project_uses() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Member).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            project: None,
            owner_team: Some(team_id()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;

    s.server
        .get(&format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/deliveries",
            connection.id
        ))
        .add_header("authorization", &s.auth)
        .await
        .assert_status_ok();
    s.db.finish().await;
}

fn instance_repository_grants_path(connection: GitHostConnectionId) -> String {
    format!("/api/v1/admin/git-host-connections/{connection}/repository-grants")
}

async fn set_test_user_as_superuser(s: &RealTestDatabaseHttpServerAndBearerAuthHeader) {
    s.db.execute(&format!(
        "UPDATE \"user\" SET superuser = true WHERE id = '{}'",
        user_id()
    ))
    .await;
}

async fn create_instance_repository_grant(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
    body: Value,
) -> axum_test::TestResponse {
    s.server
        .put(&instance_repository_grants_path(connection))
        .add_header("authorization", &s.auth)
        .json(&body)
        .await
}

async fn set_repository_then_attach_connection_to_test_task(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
    repository: &str,
) -> axum_test::TestResponse {
    s.db.execute(&format!(
        "UPDATE task SET repository = '{repository}', git_host_connection = NULL WHERE id = '{}'",
        gradient_fixtures::ids::task_id()
    ))
    .await;
    s.server
        .patch("/api/v1/tasks/test-project/test-task/repository")
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.to_string(), "report_status": false }))
        .await
}

async fn read_all_stored_repository_grants(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> Vec<gradient_entity::git_host_connection_repository_grant::Model> {
    s.db.rows::<gradient_entity::git_host_connection_repository_grant::Entity>()
        .await
}

#[tokio::test]
async fn project_admin_who_is_not_superuser_is_refused_when_listing_or_creating_instance_repository_grants()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;

    let created = create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await;
    let listed = s
        .server
        .get(&instance_repository_grants_path(instance.id))
        .add_header("authorization", &s.auth)
        .await;

    created.assert_status_forbidden();
    listed.assert_status_forbidden();
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn test_superuser_created_instance_repository_grant_lets_task_use_connection_only_for_granted_repository()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let body =
        json!({ "project": "test-project", "repository_owner": "Acme", "repository_name": "App" });

    let created = create_instance_repository_grant(&s, instance.id, body.clone()).await;
    let created_again = create_instance_repository_grant(&s, instance.id, body).await;
    let granted = set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/app.git",
    )
    .await;
    let other_repository = set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/other.git",
    )
    .await;
    let listed = s
        .server
        .get(&instance_repository_grants_path(instance.id))
        .add_header("authorization", &s.auth)
        .await;

    created.assert_status_ok();
    created_again.assert_status_conflict();
    granted.assert_status_ok();
    other_repository.assert_status_forbidden();
    listed.assert_status_ok();
    let listed = listed.json::<Value>()["message"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["project_name"], "test-project");
    assert_eq!(listed[0]["source"], "admin");
    let stored = read_all_stored_repository_grants(&s).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].repository_owner, "acme");
    assert_eq!(stored[0].repository_name.as_deref(), Some("app"));
    assert_eq!(stored[0].created_by, Some(user_id()));
    s.db.finish().await;
}

#[tokio::test]
async fn owner_wide_instance_repository_grant_covers_every_repository_of_owner_and_no_repository_of_other_owner()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme" }),
    )
    .await
    .assert_status_ok();
    let second_repository = set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/other.git",
    )
    .await;
    let other_owner = set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:someone-else/app.git",
    )
    .await;

    second_repository.assert_status_ok();
    other_owner.assert_status_forbidden();
    s.db.finish().await;
}

#[tokio::test]
async fn deleting_instance_repository_grant_refuses_next_attach_and_writes_create_and_delete_audit_events()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let created = create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await;
    created.assert_status_ok();
    let grant = created.json::<Value>()["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/app.git",
    )
    .await
    .assert_status_ok();

    let deleted = s
        .server
        .delete(&format!(
            "{}/{grant}",
            instance_repository_grants_path(instance.id)
        ))
        .add_header("authorization", &s.auth)
        .await;
    let after_delete = set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/app.git",
    )
    .await;

    deleted.assert_status_ok();
    after_delete.assert_status_forbidden();
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    let mut events: Vec<(chrono::NaiveDateTime, String)> =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| {
                row.event
                    .starts_with("git_host_connection.repository_grant.")
            })
            .map(|row| (row.created_at, row.event))
            .collect();
    events.sort();
    let events: Vec<String> = events.into_iter().map(|(_, event)| event).collect();
    assert_eq!(
        events,
        [
            "git_host_connection.repository_grant.create",
            "git_host_connection.repository_grant.delete"
        ]
    );
    s.db.finish().await;
}

#[tokio::test]
async fn instance_repository_grant_with_owner_or_repository_not_a_plain_path_segment_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let refused_bodies = [
        json!({ "project": "test-project", "repository_owner": "acme/app" }),
        json!({ "project": "test-project", "repository_owner": ".." }),
        json!({ "project": "test-project", "repository_owner": " acme" }),
        json!({ "project": "test-project", "repository_owner": "" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app//x" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "/app" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app/" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "sub/../app" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "sub/./app" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": ".." }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "a pp" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app.git" }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "sub/app.git" }),
        json!({ "project": "test-project", "repository_owner": "a".repeat(256) }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "a".repeat(256) }),
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": format!("{}/app", "a".repeat(252)) }),
    ];

    for body in refused_bodies {
        let status = create_instance_repository_grant(&s, instance.id, body.clone())
            .await
            .status_code();
        assert!(
            matches!(status.as_u16(), 400 | 422),
            "{body} answered {status}"
        );
    }

    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn test_instance_repository_grant_for_gitlab_nested_group_repository_covers_only_that_repository()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;

    let created = create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "group", "repository_name": "Sub/Repo" }),
    )
    .await;
    created.assert_status_ok();
    let mut answers = Vec::new();
    for repository in [
        "git@gitea.example.com:group/sub/repo.git",
        "git@gitea.example.com:group/sub/repo2.git",
        "git@gitea.example.com:group/sub.git",
        "git@gitea.example.com:group/other/repo.git",
        "git@gitea.example.com:group/sub/repo/deeper.git",
    ] {
        let status =
            set_repository_then_attach_connection_to_test_task(&s, instance.id, repository)
                .await
                .status_code()
                .as_u16();
        answers.push((repository, status));
    }

    assert_eq!(
        answers,
        [
            ("git@gitea.example.com:group/sub/repo.git", 200),
            ("git@gitea.example.com:group/sub/repo2.git", 403),
            ("git@gitea.example.com:group/sub.git", 403),
            ("git@gitea.example.com:group/other/repo.git", 403),
            ("git@gitea.example.com:group/sub/repo/deeper.git", 403),
        ]
    );
    let stored = read_all_stored_repository_grants(&s).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].repository_owner, "group");
    assert_eq!(stored[0].repository_name.as_deref(), Some("sub/repo"));
    s.db.finish().await;
}

#[tokio::test]
async fn instance_repository_grant_over_a_state_file_or_upgrade_grant_takes_it_over_as_admin_grant()
{
    use gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource;
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    for (name, source) in [
        ("from-state-file", RepositoryGrantSource::StateFile),
        ("from-upgrade", RepositoryGrantSource::Upgrade),
    ] {
        gradient_db::git_host_connection_repository_grants::insert(
            &s.db.connection(),
            gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
                connection: instance.id,
                project: project_id(),
                repository_owner: "acme",
                repository_name: Some(name),
                source,
                created_by: None,
            },
        )
        .await
        .unwrap();
    }
    let before = read_all_stored_repository_grants(&s).await;

    let mut answers = Vec::new();
    for name in ["From-State-File", "from-upgrade"] {
        let body = json!({ "project": "test-project", "repository_owner": "Acme", "repository_name": name });
        let created = create_instance_repository_grant(&s, instance.id, body.clone()).await;
        let created_again = create_instance_repository_grant(&s, instance.id, body).await;
        answers.push((
            created.status_code().as_u16(),
            created.json::<Value>()["message"]["source"].clone(),
            created_again.status_code().as_u16(),
        ));
    }

    assert_eq!(
        answers,
        [(200, json!("admin"), 409), (200, json!("admin"), 409)]
    );
    let stored = read_all_stored_repository_grants(&s).await;
    assert_eq!(stored.len(), 2);
    for grant in &stored {
        let former = before.iter().find(|row| row.id == grant.id).unwrap();
        assert_eq!(grant.repository_name, former.repository_name);
        assert_eq!(grant.source, RepositoryGrantSource::Admin);
        assert_eq!(grant.created_by, Some(user_id()));
    }
    let mut taken_over_from: Vec<Value> =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| row.event == "git_host_connection.repository_grant.create")
            .map(|row| row.metadata.unwrap()["taken_over_from_source"].clone())
            .collect();
    taken_over_from.sort_by_key(Value::to_string);
    assert_eq!(taken_over_from, [json!("state_file"), json!("upgrade")]);
    s.db.finish().await;
}

async fn insert_task_and_return_id(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    project: ProjectId,
    name: &str,
    repository: &str,
) -> TaskId {
    gradient_db::rows::insert::<task::Entity, _>(
        &s.db.connection(),
        task::Model {
            id: TaskId::now_v7(),
            project,
            name: name.into(),
            repository: repository.into(),
            ..rows::task()
        },
    )
    .await
    .unwrap()
    .id
}

async fn read_git_host_connection_id_from_task_row(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    task: TaskId,
) -> Option<GitHostConnectionId> {
    s.db.find::<task::Entity>(task)
        .await
        .unwrap()
        .git_host_connection
}

#[tokio::test]
async fn test_superuser_repository_grant_links_task_without_connection_copies_reports_status_and_lists_linked_task_in_create_audit()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();

    let task =
        s.db.find::<task::Entity>(gradient_fixtures::ids::task_id())
            .await
            .unwrap();
    assert_eq!(
        task.git_host_connection,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    assert_eq!(task.report_status, instance.reports_status);
    let linked: Vec<Value> =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| row.event == "git_host_connection.repository_grant.create")
            .map(|row| row.metadata.unwrap()["git_host_connection_linked_task_ids"].clone())
            .collect();
    assert_eq!(
        linked,
        [json!([gradient_fixtures::ids::task_id().to_string()])],
        "Audit events for creation list the IDs of linked tasks."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn owner_wide_grant_links_only_project_tasks_in_repositories_owned_by_grant_owner() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let lib = insert_task_and_return_id(
        &s,
        project_id(),
        "lib",
        "git@gitea.example.com:acme/lib.git",
    )
    .await;
    let other_owner = insert_task_and_return_id(
        &s,
        project_id(),
        "other-owner",
        "git@gitea.example.com:other-owner/x.git",
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme" }),
    )
    .await
    .assert_status_ok();

    for task in [gradient_fixtures::ids::task_id(), lib] {
        assert_eq!(
            read_git_host_connection_id_from_task_row(&s, task).await,
            Some(instance.id),
            "Create a repository grant for the task repository. Tasks without a connection link to that connection."
        );
    }
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, other_owner).await,
        None,
        "A task with a repository outside the grant doesn't keep a connection."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn owner_wide_grant_links_only_tasks_of_that_project_and_keeps_other_project_task_unconnected()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let target = insert_project_named_target_project_and_add_test_user_as_admin_member(&s).await;
    let other_project = insert_task_and_return_id(
        &s,
        target.id,
        "test-task",
        "git@gitea.example.com:acme/app.git",
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, other_project).await,
        None,
        "A task with a repository outside the grant doesn't keep a connection."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn second_repository_grant_for_acme_lib_links_acme_lib_task_but_not_existing_acme_app_task_and_audit_event_lists_only_acme_lib_task()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();
    let app = insert_task_and_return_id(
        &s,
        project_id(),
        "app",
        "git@gitea.example.com:acme/app.git",
    )
    .await;
    let lib = insert_task_and_return_id(
        &s,
        project_id(),
        "lib",
        "git@gitea.example.com:acme/lib.git",
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "lib" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, app).await,
        None,
        "A task can stay without a connection when earlier grants cover only one of its repositories, even after creation of a grant for another repository."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, lib).await,
        Some(instance.id),
        "Tasks without a connection link to a new grant for their repository."
    );
    let linked: Vec<Value> =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| row.event == "git_host_connection.repository_grant.create")
            .map(|row| row.metadata.unwrap()["git_host_connection_linked_task_ids"].clone())
            .collect();
    assert_eq!(
        linked,
        [
            json!([gradient_fixtures::ids::task_id().to_string()]),
            json!([lib.to_string()])
        ],
        "Audit events for new grants list only the tasks that the grant linked."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn grant_in_other_project_does_not_link_task_in_target_project() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let target = insert_project_named_target_project_and_add_test_user_as_admin_member(&s).await;
    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "target-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();
    let target_task =
        insert_task_and_return_id(&s, target.id, "app", "git@gitea.example.com:acme/app.git").await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "lib" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, target_task).await,
        None,
        "A grant for one project doesn't link tasks from another project at all."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn named_grant_for_acme_app_connects_task_to_acme_app_and_not_sibling_acme_lib() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let lib = insert_task_and_return_id(
        &s,
        project_id(),
        "lib",
        "git@gitea.example.com:acme/lib.git",
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, lib).await,
        None,
        "A task with a repository outside the grant doesn't keep a connection."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn creating_repository_grant_for_acme_app_links_only_task_on_connection_host() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let on_connection_host = insert_task_and_return_id(
        &s,
        project_id(),
        "on-connection-host",
        "git@gitea.example.com:acme/app.git",
    )
    .await;
    let on_other_host = insert_task_and_return_id(
        &s,
        project_id(),
        "on-other-host",
        "git@forgejo.example.org:acme/app.git",
    )
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, on_connection_host).await,
        Some(instance.id),
        "A grant can link tasks that have a repository on the connected host."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, on_other_host).await,
        None,
        "The grant does not link a task on another host, even when the owner name and the repository name are the same."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn creating_grant_keeps_existing_task_connection_and_links_task_without_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let project_connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    let with_connection = insert_task_and_return_id(
        &s,
        project_id(),
        "with-connection",
        "git@gitea.example.com:acme/app.git",
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}' WHERE id = '{with_connection}'",
        project_connection.id
    ))
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, with_connection).await,
        Some(project_connection.id),
        "Tasks with an existing connection keep it even after grant creation."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn managed_task_is_not_linked_by_grant_creation_but_unmanaged_task_with_same_repository_is() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let managed = insert_task_and_return_id(
        &s,
        project_id(),
        "managed",
        "git@gitea.example.com:acme/app.git",
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET managed = true WHERE id = '{managed}'"
    ))
    .await;

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    let managed = s.db.find::<task::Entity>(managed).await.unwrap();
    assert_eq!(
        managed.git_host_connection, None,
        "Tasks managed in the state file remain without a connection to a Git host after grant creation."
    );
    assert!(!managed.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn team_admin_with_view_only_project_role_creates_team_connection_grant_and_task_is_linked() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;

    send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        repository_grant_request_body_for_test_project_task_repository(),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(connection.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn test_delete_grant_keeps_task_linked_to_connection_and_rejects_new_attach() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    let created = create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await;
    created.assert_status_ok();
    let grant = created.json::<Value>()["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );

    s.server
        .delete(&format!(
            "{}/{grant}",
            instance_repository_grants_path(instance.id)
        ))
        .add_header("authorization", &s.auth)
        .await
        .assert_status_ok();

    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, gradient_fixtures::ids::task_id()).await,
        Some(instance.id),
        "Grant deletion does not remove the link between the task and the connection."
    );
    set_repository_then_attach_connection_to_test_task(
        &s,
        instance.id,
        "git@gitea.example.com:acme/app.git",
    )
    .await
    .assert_status_forbidden();
    s.db.finish().await;
}

#[tokio::test]
async fn taking_over_state_file_grant_through_api_does_not_link_task_but_new_grant_does() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let instance = insert_test_git_host_connection(
        &s,
        make_instance_gitea_connection_for_task_repository_host(),
    )
    .await;
    gradient_db::git_host_connection_repository_grants::insert(
        &s.db.connection(),
        gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
            connection: instance.id,
            project: project_id(),
            repository_owner: "acme",
            repository_name: Some("app"),
            source:
                gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource::StateFile,
            created_by: None,
        },
    )
    .await
    .unwrap();
    let lib = insert_task_and_return_id(
        &s,
        project_id(),
        "lib",
        "git@gitea.example.com:acme/lib.git",
    )
    .await;
    let mut before = s.db.rows::<task::Entity>().await;
    before.sort_by_key(|task| task.id);

    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" }),
    )
    .await
    .assert_status_ok();
    let mut after_take_over = s.db.rows::<task::Entity>().await;
    after_take_over.sort_by_key(|task| task.id);
    create_instance_repository_grant(
        &s,
        instance.id,
        json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "lib" }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        after_take_over, before,
        "A take-over of a grant from the state file does not change the task row."
    );
    assert_eq!(
        read_git_host_connection_id_from_task_row(&s, lib).await,
        Some(instance.id),
        "Create a repository grant for the task repository. Tasks without a connection link to that connection."
    );
    s.db.finish().await;
}

#[tokio::test]
async fn instance_repository_grant_endpoints_refuse_a_project_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    let project_connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;

    create_instance_repository_grant(
        &s,
        project_connection.id,
        json!({ "project": "test-project", "repository_owner": "acme" }),
    )
    .await
    .assert_status_not_found();

    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

fn team_git_host_connection_repository_grants_path(
    team: &str,
    connection: GitHostConnectionId,
) -> String {
    format!("/api/v1/teams/{team}/git-host-connections/{connection}/repository-grants")
}

fn team_connection_for_task_repository_host() -> git_host_connection::Model {
    git_host_connection::Model {
        owner_team: Some(team_id()),
        ..make_instance_gitea_connection_for_task_repository_host()
    }
}

async fn send_put_team_repository_grant_request(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    team: &str,
    connection: GitHostConnectionId,
    body: Value,
) -> axum_test::TestResponse {
    s.server
        .put(&team_git_host_connection_repository_grants_path(
            team, connection,
        ))
        .add_header("authorization", &s.auth)
        .json(&body)
        .await
}

async fn send_get_team_repository_grants_request(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    team: &str,
    connection: GitHostConnectionId,
) -> axum_test::TestResponse {
    s.server
        .get(&team_git_host_connection_repository_grants_path(
            team, connection,
        ))
        .add_header("authorization", &s.auth)
        .await
}

fn repository_grant_request_body_for_test_project_task_repository() -> Value {
    json!({ "project": "test-project", "repository_owner": "acme", "repository_name": "app" })
}

#[tokio::test]
async fn team_member_who_is_not_team_admin_is_refused_when_listing_or_creating_team_connection_repository_grants()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Member).await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;

    let created = send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        repository_grant_request_body_for_test_project_task_repository(),
    )
    .await;
    let listed = send_get_team_repository_grants_request(&s, "platform", connection.id).await;

    created.assert_status_forbidden();
    listed.assert_status_forbidden();
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn admin_of_different_team_cannot_list_create_or_delete_repository_grants_of_team_connection()
{
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let conn = s.db.connection();
    gradient_db::rows::insert::<gradient_entity::team::Entity, _>(&conn, rows::team())
        .await
        .unwrap();
    let other_team = gradient_entity::team::Model {
        id: TeamId::now_v7(),
        name: "other-team".into(),
        ..rows::team()
    };
    gradient_db::rows::insert::<gradient_entity::team::Entity, _>(&conn, other_team.clone())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::team_user::Entity, _>(
        &conn,
        gradient_entity::team_user::Model {
            team: other_team.id,
            ..rows::team_member(TeamRole::Admin)
        },
    )
    .await
    .unwrap();
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;
    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        connection.id,
        project_id(),
    )
    .await;
    let existing = read_all_stored_repository_grants(&s).await;

    for team in ["other-team", "platform"] {
        let created = send_put_team_repository_grant_request(
            &s,
            team,
            connection.id,
            json!({ "project": "test-project", "repository_owner": "acme" }),
        )
        .await;
        let listed = send_get_team_repository_grants_request(&s, team, connection.id).await;
        let deleted = s
            .server
            .delete(&format!(
                "{}/{}",
                team_git_host_connection_repository_grants_path(team, connection.id),
                existing[0].id
            ))
            .add_header("authorization", &s.auth)
            .await;

        created.assert_status_not_found();
        listed.assert_status_not_found();
        deleted.assert_status_not_found();
    }

    assert_eq!(read_all_stored_repository_grants(&s).await, existing);
    s.db.finish().await;
}

#[tokio::test]
async fn team_admin_repository_grant_allows_project_task_to_attach_team_connection_until_deleted() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;
    let repository = "git@gitea.example.com:acme/app.git";

    let before_grant =
        set_repository_then_attach_connection_to_test_task(&s, connection.id, repository).await;
    let created = send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        repository_grant_request_body_for_test_project_task_repository(),
    )
    .await;
    let granted =
        set_repository_then_attach_connection_to_test_task(&s, connection.id, repository).await;
    let listed = send_get_team_repository_grants_request(&s, "platform", connection.id).await;

    before_grant.assert_status_forbidden();
    created.assert_status_ok();
    granted.assert_status_ok();
    listed.assert_status_ok();
    let listed = listed.json::<Value>()["message"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["project_name"], "test-project");
    assert_eq!(listed[0]["source"], "admin");
    let grant = created.json::<Value>()["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let deleted = s
        .server
        .delete(&format!(
            "{}/{grant}",
            team_git_host_connection_repository_grants_path("platform", connection.id)
        ))
        .add_header("authorization", &s.auth)
        .await;
    let after_delete =
        set_repository_then_attach_connection_to_test_task(&s, connection.id, repository).await;

    deleted.assert_status_ok();
    after_delete.assert_status_forbidden();
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    let events =
        s.db.rows::<gradient_entity::audit_log::Entity>()
            .await
            .into_iter()
            .filter(|row| {
                row.event
                    .starts_with("git_host_connection.repository_grant.")
            })
            .count();
    assert_eq!(events, 2);
    s.db.finish().await;
}

#[tokio::test]
async fn team_repository_grant_list_hides_the_name_of_a_project_that_left_the_team_and_still_deletes_its_grant()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;
    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        connection.id,
        project_id(),
    )
    .await;
    s.db.execute(&format!(
        "UPDATE project SET owner_team = NULL WHERE id = '{}'",
        project_id()
    ))
    .await;

    let listed = send_get_team_repository_grants_request(&s, "platform", connection.id).await;
    listed.assert_status_ok();
    let listed = listed.json::<Value>()["message"].clone();
    let deleted = s
        .server
        .delete(&format!(
            "{}/{}",
            team_git_host_connection_repository_grants_path("platform", connection.id),
            listed[0]["id"].as_str().unwrap()
        ))
        .add_header("authorization", &s.auth)
        .await;

    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["project"], project_id().to_string());
    assert_eq!(listed[0]["project_name"], Value::Null);
    deleted.assert_status_ok();
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn team_repository_grant_outside_team_or_missing_project_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    insert_project_named_target_project_and_add_test_user_as_admin_member(&s).await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;

    let outside_team = send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        json!({ "project": "target-project", "repository_owner": "acme" }),
    )
    .await;
    let missing = send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        json!({ "project": "no-such-project", "repository_owner": "acme" }),
    )
    .await;

    outside_team.assert_status_bad_request();
    missing.assert_status_bad_request();
    assert_eq!(
        error_message_from_response(&outside_team).replace("target-project", "no-such-project"),
        error_message_from_response(&missing)
    );
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn superuser_not_in_team_can_manage_team_repository_grants_like_other_team_connection_endpoints()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    set_test_user_as_superuser(&s).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Member).await;
    s.db.execute(&format!(
        "DELETE FROM team_user WHERE \"user\" = '{}'",
        user_id()
    ))
    .await;
    let connection =
        insert_test_git_host_connection(&s, team_connection_for_task_repository_host()).await;

    let created = send_put_team_repository_grant_request(
        &s,
        "platform",
        connection.id,
        repository_grant_request_body_for_test_project_task_repository(),
    )
    .await;
    let listed = send_get_team_repository_grants_request(&s, "platform", connection.id).await;

    created.assert_status_ok();
    listed.assert_status_ok();
    assert_eq!(read_all_stored_repository_grants(&s).await.len(), 1);
    s.db.finish().await;
}

const GITEA_OR_FORGEJO_OAUTH_CONNECTION_COMPLETE_REST_PATH: &str =
    "/api/v1/projects/test-project/git-host-connections/oauth/complete";

struct TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles {
    s: RealTestDatabaseHttpServerAndBearerAuthHeader,
    git_host: MockServer,
}

async fn create_test_project_with_git_host_oauth_application(
    member_role: role::Model,
    expected_token_requests: u64,
) -> TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles {
    create_project_with_git_host_oauth_application_and_tls_setting(
        member_role,
        expected_token_requests,
        false,
    )
    .await
}

async fn create_project_with_git_host_oauth_application_and_tls_setting(
    member_role: role::Model,
    expected_token_requests: u64,
    use_tls: bool,
) -> TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles {
    let git_host = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
            "token_type": "bearer",
        })))
        .expect(expected_token_requests)
        .mount(&git_host)
        .await;
    let client_secret_file =
        std::env::temp_dir().join(format!("gradient-test-gitea-app-{}", Uuid::now_v7()));
    std::fs::write(&client_secret_file, "s3cret\n").unwrap();
    let instance_base_url = git_host.uri();
    let s = create_test_database_with_project_membership_role_and_server_settings(
        member_role,
        move |state| {
            state.cli(move |cli| {
                cli.gitea_app.instance_base_url = Some(instance_base_url);
                cli.gitea_app.client_id = Some("gradient-client".into());
                cli.gitea_app.client_secret_file =
                    Some(client_secret_file.to_string_lossy().into_owned());
                cli.server.use_tls = use_tls;
            })
        },
    )
    .await;
    TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles { s, git_host }
}

#[tokio::test]
async fn test_expired_forgejo_access_token_connection_test_refreshes_token_succeeds_with_account_name_and_stores_new_token()
 {
    let TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles { s, git_host } =
        create_test_project_with_git_host_oauth_application(rows::admin_role(), 1).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "octo" })))
        .expect(1)
        .mount(&git_host)
        .await;
    let encrypt = |plain: &str| {
        gradient_ci::actions::encrypt_secret_with_file(&s.secret_encryption_key_file_path, plain)
            .unwrap()
    };
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            git_host_type: GitHostType::Forgejo,
            access_token: Some(encrypt("old-access")),
            encrypted_refresh_token: Some(encrypt("old-refresh")),
            access_token_expires_at: Some(test_date()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                &git_host.uri(),
                false,
            )
        },
    )
    .await;

    let res = test_project_connection(&s, connection.id).await;

    res.assert_status_ok();
    let result = res.json::<Value>()["message"].clone();
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["account"], "octo");
    let stored =
        s.db.find::<git_host_connection::Entity>(connection.id)
            .await
            .unwrap();
    let token = gradient_ci::actions::decrypt_secret_with_file(
        &s.secret_encryption_key_file_path,
        stored.access_token.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(token.expose(), "access");
    git_host.verify().await;
    s.db.finish().await;
}

fn build_signed_git_host_oauth_connect_cookie_header_value(
    session: SessionId,
    user: UserId,
    project: ProjectId,
) -> String {
    let jwt = git_host_oauth_connect_cookie_claims_signed_with_signing_key(
        TEST_JWT_SECRET,
        session,
        user,
        project,
    );
    format!("git_host_oauth_connect={jwt}")
}

fn git_host_oauth_connect_cookie_claims_signed_with_signing_key(
    secret: &str,
    session: SessionId,
    user: UserId,
    project: ProjectId,
) -> String {
    let now = chrono::Utc::now().timestamp();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &json!({
            "exp": now + 600,
            "iat": now,
            "state": "s",
            "pkce_verifier": "v",
            "user_id": user,
            "session_id": session,
            "project_id": project,
            "project_name": "test-project",
            "name": "codeberg",
            "display_name": "Codeberg",
        }),
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn post_complete_git_host_oauth_connection_with_test_bearer_auth_connect_cookie_and_state(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    cookie: &str,
) -> axum_test::TestResponse {
    post_git_host_oauth_connection_completion_with_authorization_header_connect_cookie_and_state(
        s,
        &s.auth,
        Some(cookie),
        "s",
    )
    .await
}

async fn post_git_host_oauth_connection_completion_with_authorization_header_connect_cookie_and_state(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    authorization: &str,
    cookie: Option<&str>,
    state: &str,
) -> axum_test::TestResponse {
    let mut request = s
        .server
        .post(GITEA_OR_FORGEJO_OAUTH_CONNECTION_COMPLETE_REST_PATH)
        .add_header("authorization", authorization);
    if let Some(cookie) = cookie {
        request = request.add_header("cookie", cookie);
    }
    request
        .json(&json!({ "code": "abc", "state": state }))
        .await
}

async fn read_stored_codeberg_git_host_connection_rows(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> Vec<git_host_connection::Model> {
    s.db.rows::<git_host_connection::Entity>()
        .await
        .into_iter()
        .filter(|row| row.name == "codeberg")
        .collect()
}

async fn post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(
    setup: TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles,
    cookie: &str,
    expected_status: u16,
) {
    let res =
        post_complete_git_host_oauth_connection_with_test_bearer_auth_connect_cookie_and_state(
            &setup.s, cookie,
        )
        .await;
    assert_refusal_stores_no_connection_and_sends_no_git_host_token_request(
        setup,
        res,
        expected_status,
    )
    .await;
}

async fn assert_refusal_stores_no_connection_and_sends_no_git_host_token_request(
    setup: TestProjectWithGitHostOAuthApplicationAndCompletionTestHandles,
    res: axum_test::TestResponse,
    expected_status: u16,
) {
    assert_eq!(
        res.status_code().as_u16(),
        expected_status,
        "{}",
        res.text()
    );
    assert!(
        read_stored_codeberg_git_host_connection_rows(&setup.s)
            .await
            .is_empty()
    );
    setup.git_host.verify().await;
    setup.s.db.finish().await;
}

#[tokio::test]
async fn oauth2_callback_endpoint_redirects_to_project_page_and_stores_no_connection() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    let res = setup
        .s
        .server
        .get("/api/v1/git-host-connections/oauth/callback?code=a%2Bc&state=s")
        .add_header("cookie", &cookie)
        .await;

    res.assert_status(axum::http::StatusCode::FOUND);
    assert_eq!(
        res.header("location").to_str().unwrap(),
        "/project/test-project/git-host-connections?code=a%2Bc&state=s"
    );
    assert!(
        read_stored_codeberg_git_host_connection_rows(&setup.s)
            .await
            .is_empty()
    );
    setup.git_host.verify().await;
    setup.s.db.finish().await;
}

#[tokio::test]
async fn completing_oauth2_connection_stores_exactly_one_git_host_connection_for_project() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 1).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    let res =
        post_complete_git_host_oauth_connection_with_test_bearer_auth_connect_cookie_and_state(
            &setup.s, &cookie,
        )
        .await;

    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"]["name"], "codeberg");
    assert!(
        res.header("set-cookie")
            .to_str()
            .unwrap()
            .starts_with("git_host_oauth_connect=;")
    );
    let stored = read_stored_codeberg_git_host_connection_rows(&setup.s).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].project, Some(project_id()));
    assert_eq!(stored[0].created_by, user_id());
    let audited = setup
        .s
        .db
        .rows::<gradient_entity::audit_log::Entity>()
        .await
        .into_iter()
        .filter(|row| row.event == "git_host_connection.create")
        .count();
    assert_eq!(audited, 1);
    setup.git_host.verify().await;
    setup.s.db.finish().await;
}

#[tokio::test]
async fn completing_with_the_connect_cookie_of_another_session_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        SessionId::now_v7(),
        user_id(),
        project_id(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 400).await;
}

#[tokio::test]
async fn completing_with_the_connect_cookie_of_another_user_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        UserId::now_v7(),
        project_id(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 400).await;
}

#[tokio::test]
async fn completing_with_the_connect_cookie_of_another_project_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        ProjectId::now_v7(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 400).await;
}

#[tokio::test]
async fn oauth2_connection_completion_by_project_member_without_git_host_connection_permission_is_refused_and_stores_nothing()
 {
    let setup = create_test_project_with_git_host_oauth_application(rows::view_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 403).await;
}

#[tokio::test]
async fn sending_completion_request_twice_is_refused_and_sends_no_second_token_request_to_git_host()
{
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 1).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );
    post_complete_git_host_oauth_connection_with_test_bearer_auth_connect_cookie_and_state(
        &setup.s, &cookie,
    )
    .await
    .assert_status_ok();

    let replayed =
        post_complete_git_host_oauth_connection_with_test_bearer_auth_connect_cookie_and_state(
            &setup.s, &cookie,
        )
        .await;

    replayed.assert_status(axum::http::StatusCode::CONFLICT);
    assert_eq!(
        read_stored_codeberg_git_host_connection_rows(&setup.s)
            .await
            .len(),
        1
    );
    setup.git_host.verify().await;
    setup.s.db.finish().await;
}

#[tokio::test]
async fn completing_with_a_state_different_from_the_connect_cookie_state_is_refused_and_stores_nothing()
 {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    let res =
        post_git_host_oauth_connection_completion_with_authorization_header_connect_cookie_and_state(&setup.s, &setup.s.auth, Some(&cookie), "other")
            .await;

    assert_refusal_stores_no_connection_and_sends_no_git_host_token_request(setup, res, 400).await;
}

#[tokio::test]
async fn completing_oauth2_connection_without_connect_cookie_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;

    let res = post_git_host_oauth_connection_completion_with_authorization_header_connect_cookie_and_state(&setup.s, &setup.s.auth, None, "s").await;

    assert_refusal_stores_no_connection_and_sends_no_git_host_token_request(setup, res, 400).await;
}

#[tokio::test]
async fn completing_with_connect_cookie_signed_with_different_key_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let forged = git_host_oauth_connect_cookie_claims_signed_with_signing_key(
        "a-key-that-is-not-the-server-key",
        setup.s.session,
        user_id(),
        project_id(),
    );
    let cookie = format!("git_host_oauth_connect={forged}");

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 400).await;
}

#[tokio::test]
async fn completing_with_same_user_api_key_with_every_permission_is_refused_and_stores_nothing() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let raw_api_key = gradient_web::authorization::generate_api_key();
    let now = chrono::Utc::now().naive_utc();
    gradient_db::rows::insert::<gradient_entity::api::Entity, _>(
        &setup.s.db.connection(),
        gradient_entity::api::Model {
            id: ApiId::now_v7(),
            owned_by: user_id(),
            name: "every-permission".into(),
            key: gradient_web::authorization::hash_api_key(&raw_api_key),
            last_used_at: now,
            created_at: now,
            permission: gradient_types::permissions::admin_mask(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    let res = post_git_host_oauth_connection_completion_with_authorization_header_connect_cookie_and_state(
        &setup.s,
        &format!("Bearer GRAD{raw_api_key}"),
        Some(&cookie),
        "s",
    )
    .await;

    assert_refusal_stores_no_connection_and_sends_no_git_host_token_request(setup, res, 401).await;
}

#[tokio::test]
async fn completing_after_sign_out_of_the_browser_session_that_started_the_connection_is_refused() {
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );
    setup
        .s
        .server
        .post("/api/v1/auth/logout")
        .add_header("authorization", &setup.s.auth)
        .await
        .assert_status_ok();

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 401).await;
}

#[tokio::test]
async fn completing_oauth2_connection_for_state_file_managed_project_is_refused_and_stores_nothing()
{
    let setup = create_test_project_with_git_host_oauth_application(rows::admin_role(), 0).await;
    setup
        .s
        .db
        .execute("UPDATE project SET managed = true")
        .await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 403).await;
}

#[tokio::test]
async fn completing_under_tls_ignores_connect_cookie_without_host_prefix() {
    let setup =
        create_project_with_git_host_oauth_application_and_tls_setting(rows::admin_role(), 0, true)
            .await;
    let cookie = build_signed_git_host_oauth_connect_cookie_header_value(
        setup.s.session,
        user_id(),
        project_id(),
    );

    post_completion_request_with_connect_cookie_and_assert_refused_without_stored_connection_or_token_request(setup, &cookie, 400).await;
}

async fn insert_project_with_name_and_team_returning_id(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    name: &str,
    owner_team: Option<TeamId>,
) -> ProjectId {
    let id = ProjectId::now_v7();
    gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        project::Model {
            id,
            name: name.into(),
            owner_team,
            public: false,
            ..rows::project()
        },
    )
    .await
    .unwrap();
    id
}

async fn insert_unmanaged_task_without_git_host_connection_and_return_id(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    project: ProjectId,
    name: &str,
    repository: &str,
) -> TaskId {
    let id = TaskId::now_v7();
    gradient_db::rows::insert::<task::Entity, _>(
        &s.db.connection(),
        task::Model {
            id,
            project,
            name: name.into(),
            repository: repository.into(),
            ..rows::task()
        },
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn new_instance_wide_github_app_installation_does_not_link_unmanaged_tasks_and_creates_no_repository_grant()
 {
    use hmac::{Hmac, KeyInit, Mac};
    let secret = "github-webhook-secret";
    let secret_file = std::env::temp_dir().join(format!("gradient-test-secret-{}", Uuid::now_v7()));
    std::fs::write(&secret_file, secret).unwrap();
    let s = create_test_database_with_project_membership_role_and_server_settings(
        rows::admin_role(),
        |state| {
            state.cli(|cli| {
                cli.github_app.id = std::num::NonZeroU64::new(1234);
                cli.github_app.private_key_file = Some("/dev/null".into());
                cli.github_app.webhook_secret_file =
                    Some(secret_file.to_string_lossy().into_owned());
            })
        },
    )
    .await;
    set_test_user_as_superuser(&s).await;
    s.db.execute("UPDATE task SET repository = 'https://github.com/acme/infra'")
        .await;
    let other_project =
        insert_project_with_name_and_team_returning_id(&s, "other-project", None).await;
    let other_task = insert_unmanaged_task_without_git_host_connection_and_return_id(
        &s,
        other_project,
        "other-task",
        "https://github.com/acme/infra",
    )
    .await;
    let body = serde_json::to_vec(&json!({
        "action": "created",
        "installation": { "id": 9999, "account": { "login": "acme" } },
        "sender": { "login": "some-user" },
    }))
    .unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    s.server
        .post("/api/v1/hooks/github")
        .add_header("X-GitHub-Event", "installation")
        .add_header("X-Hub-Signature-256", signature)
        .bytes(body.into())
        .await
        .assert_status_ok();

    let stored = s.db.rows::<git_host_connection::Entity>().await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].github_installation_id, Some(9999));
    for id in [gradient_fixtures::ids::task_id(), other_task] {
        let task = s.db.find::<task::Entity>(id).await.unwrap();
        assert_eq!(task.git_host_connection, None, "{}", task.name);
    }
    assert!(read_all_stored_repository_grants(&s).await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn test_git_host_connection_test_endpoint_refuses_loopback_base_url_without_calling_server_or_returning_access_token()
 {
    let s = create_test_database_with_project_membership_role_and_server_settings(
        rows::admin_role(),
        |state| state.cli(|cli| cli.outbound.allowed_non_public_network_targets.clear()),
    )
    .await;
    let git_host = MockServer::start().await;
    let access_token = "plain-access-token-of-loopback-connection";
    let created = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "loopback-gitea",
            "git_host_type": "gitea",
            "base_url": git_host.uri(),
            "access_token": access_token,
            "reports_status": true,
        }))
        .await;
    created.assert_status_ok();
    let id: GitHostConnectionId = created.json::<Value>()["message"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let res = test_project_connection(&s, id).await;

    res.assert_status_ok();
    let text = res.text();
    let result = res.json::<Value>()["message"].clone();
    assert_eq!(result["success"], false, "{result}");
    assert_eq!(result["account"], Value::Null, "{result}");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .starts_with("Gradient does not call http://127.0.0.1:"),
        "{result}"
    );
    assert!(!text.contains(access_token), "{text}");
    assert!(git_host.received_requests().await.unwrap().is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn creating_project_git_host_connection_links_task_in_same_project_but_not_task_in_other_project_and_creating_team_connection_links_no_task()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Admin).await;
    let other_project =
        insert_project_with_name_and_team_returning_id(&s, "other-project", Some(team_id())).await;
    let other_task = insert_unmanaged_task_without_git_host_connection_and_return_id(
        &s,
        other_project,
        "other-task",
        "https://gitea.example.com/acme/app",
    )
    .await;
    let read_connection_of = |id: TaskId| {
        let db = &s.db;
        async move {
            db.find::<task::Entity>(id)
                .await
                .unwrap()
                .git_host_connection
        }
    };

    let project_created = s
        .server
        .put(TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
        }))
        .await;

    project_created.assert_status_ok();
    let project_created = project_created.json::<Value>()["message"].clone();
    assert_eq!(project_created["task_count"], 1, "{project_created}");
    assert_eq!(
        read_connection_of(gradient_fixtures::ids::task_id())
            .await
            .map(|id| id.to_string()),
        project_created["id"].as_str().map(str::to_owned)
    );
    assert_eq!(read_connection_of(other_task).await, None);

    s.db.execute("UPDATE task SET git_host_connection = NULL")
        .await;
    let team_created = s
        .server
        .put("/api/v1/teams/platform/git-host-connections")
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "team-gitea",
            "git_host_type": "gitea",
            "base_url": "https://gitea.example.com",
        }))
        .await;

    team_created.assert_status_ok();
    let team_created = team_created.json::<Value>()["message"].clone();
    assert_eq!(team_created["task_count"], 0, "{team_created}");
    assert_eq!(
        read_connection_of(gradient_fixtures::ids::task_id()).await,
        None
    );
    assert_eq!(read_connection_of(other_task).await, None);
    s.db.finish().await;
}

#[tokio::test]
async fn team_connection_task_lists_hide_tasks_of_private_team_project_and_task_of_other_team_project_using_repository_grant()
 {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    seed_platform_team_with_test_user_role_as_project_owner(&s, TeamRole::Member).await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            project: None,
            owner_team: Some(team_id()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                false,
            )
        },
    )
    .await;
    let private_project =
        insert_project_with_name_and_team_returning_id(&s, "team-private", Some(team_id())).await;
    insert_unmanaged_task_without_git_host_connection_and_return_id(
        &s,
        private_project,
        "private-task",
        "https://gitea.example.com/acme/app",
    )
    .await;
    let other_team = TeamId::now_v7();
    gradient_db::rows::insert::<gradient_entity::team::Entity, _>(
        &s.db.connection(),
        gradient_entity::team::Model {
            id: other_team,
            name: "other-team".into(),
            ..rows::team()
        },
    )
    .await
    .unwrap();
    let foreign_project =
        insert_project_with_name_and_team_returning_id(&s, "foreign-project", Some(other_team))
            .await;
    insert_unmanaged_task_without_git_host_connection_and_return_id(
        &s,
        foreign_project,
        "foreign-task",
        "https://gitea.example.com/acme/app",
    )
    .await;
    insert_task_repository_grant_allowing_project_to_use_git_host_connection(
        &s,
        connection.id,
        foreign_project,
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}'",
        connection.id
    ))
    .await;

    for path in [
        format!(
            "/api/v1/teams/platform/git-host-connections/{}/tasks",
            connection.id
        ),
        format!(
            "{TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH}/{}/tasks",
            connection.id
        ),
    ] {
        let res = s
            .server
            .get(&path)
            .add_header("authorization", &s.auth)
            .await;
        res.assert_status_ok();
        let tasks = res.json::<Value>()["message"].clone();
        let names: Vec<&str> = tasks
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["test-task"], "{path}: {tasks}");
    }
    s.db.finish().await;
}
