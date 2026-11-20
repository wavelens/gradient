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
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::ids::*;
use gradient_types::permissions::is_builtin_role;
use gradient_types::{GitHostType, ProjectId};
use serde_json::{Value, json};
use uuid::Uuid;

const TEST_PROJECT_GIT_HOST_CONNECTIONS_PATH: &str =
    "/api/v1/projects/test-project/git-host-connections";

struct RealTestDatabaseHttpServerAndBearerAuthHeader {
    db: TestDatabase,
    server: TestServer,
    auth: String,
}

async fn create_test_database_with_project_membership_for_role(
    member_role: role::Model,
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
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| cli.secrets.crypt_file = crypt.to_string_lossy().into_owned())
        .server();
    RealTestDatabaseHttpServerAndBearerAuthHeader {
        db,
        server,
        auth: bearer(session),
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
        .json(&json!({ "name": "gitea", "git_host_type": "gitea" }))
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
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
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
            .starts_with("Gradient doesn't call http://127.0.0.1:9"),
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
        .json(&json!({ "name": "gitea", "git_host_type": "gitea" }))
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

    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"]["task_count"], 0);
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
