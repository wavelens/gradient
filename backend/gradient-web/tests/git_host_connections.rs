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
        "Commit status reports need `base_url` and `access_token`."
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
