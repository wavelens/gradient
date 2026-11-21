/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant;
use gradient_entity::git_host_connection_repository_grant::RepositoryGrantSource;
use gradient_entity::{commit, evaluation, git_host_connection, project, role, task, team};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, task_id, team_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, spawn_loopback_http_server_with_connection_counter};
use gradient_types::ids::*;
use gradient_types::permissions::{Permission, is_builtin_role, mask_from};
use gradient_types::{GitHostType, ProjectId};
use serde_json::{Value, json};
use uuid::Uuid;

const URL: &str = "/api/v1/tasks/test-project/test-task/repository";

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

async fn read_test_task_row_from_database(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> task::Model {
    s.db.find::<task::Entity>(task_id()).await.unwrap()
}

#[tokio::test]
async fn get_task_repository_shows_selected_git_host_connection_webhook_url_and_can_edit_for_project_admin()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    s.server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.id, "report_status": true }))
        .await
        .assert_status_ok();

    let res = s.server.get(URL).add_header("authorization", &s.auth).await;

    res.assert_status_ok();
    let repo = res.json::<Value>()["message"].clone();
    assert_eq!(repo["uses_ssh"], true);
    assert!(
        repo["deploy_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519")
    );
    assert_eq!(repo["git_host_connection"]["id"], connection.id.to_string());
    assert_eq!(repo["git_host_connection"]["can_manage"], true);
    assert!(
        repo["git_host_connection"]["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/api/v1/hooks/{}", connection.id)),
        "{repo}"
    );
    assert_eq!(repo["report_status"], true);
    assert_eq!(repo["available_connections"].as_array().unwrap().len(), 1);
    assert_eq!(repo["status_posts"], json!([]));
    assert_eq!(repo["recent_deliveries"], json!([]));
    assert_eq!(repo["can_edit"], true);
    assert_eq!(repo["can_manage_integrations"], true);
    assert_eq!(repo["can_create_connection"], true);
    let chain = &repo["chain"];
    assert_eq!(chain["repository"]["state"], "ok", "{chain}");
    assert_eq!(
        chain["repository"]["detail"],
        "acme/app on gitea.example.com"
    );
    assert_eq!(chain["connection"]["state"], "ok");
    assert_eq!(chain["incoming_events"]["state"], "attention");
    assert_eq!(
        chain["incoming_events"]["action"],
        json!({ "type": "install_webhook" })
    );
    assert_eq!(chain["commit_status"]["state"], "attention");
    assert_eq!(
        chain["commit_status"]["action"],
        json!({ "type": "test_status_report" })
    );
    s.db.finish().await;
}

#[tokio::test]
async fn a_push_that_matched_no_task_offers_a_push_trigger_when_the_task_has_none() {
    let (s, connection) = database_with_task_using_a_status_reporting_connection().await;
    gradient_db::rows::insert::<gradient_entity::incoming_webhook_delivery::Entity, _>(
        &s.db.connection(),
        gradient_entity::incoming_webhook_delivery::Model {
            id: IncomingWebhookDeliveryId::now_v7(),
            git_host_connection: connection.id,
            event: "push".into(),
            status_code: 200,
            result: gradient_entity::incoming_webhook_delivery::DeliveryResult::NoMatchingTask,
            repository: Some("https://gitea.example.com/acme/app".into()),
            tasks_scanned: 1,
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let res = s.server.get(URL).add_header("authorization", &s.auth).await;

    res.assert_status_ok();
    let events = res.json::<Value>()["message"]["chain"]["incoming_events"].clone();
    assert_eq!(
        events["action"],
        json!({ "type": "add_trigger", "trigger_type": "reporter_push" }),
        "{events}"
    );
    s.db.finish().await;
}

#[tokio::test]
async fn task_repository_status_check_uses_only_newest_webhook_delivery_for_task_repository() {
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
    let conn = s.db.connection();
    gradient_db::rows::update::<task::Entity, _>(
        &conn,
        task::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(task_id()),
            git_host_connection: sea_orm::ActiveValue::Set(Some(connection.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for (repository, minutes) in [
        ("https://gitea.example.com/acme/app", 0),
        ("https://gitea.example.com/acme/other", 1),
    ] {
        gradient_db::rows::insert::<gradient_entity::incoming_webhook_delivery::Entity, _>(
            &conn,
            gradient_entity::incoming_webhook_delivery::Model {
                id: IncomingWebhookDeliveryId::now_v7(),
                git_host_connection: connection.id,
                event: "push".into(),
                status_code: 200,
                result: gradient_entity::incoming_webhook_delivery::DeliveryResult::Queued,
                repository: Some(repository.into()),
                tasks_scanned: 1,
                queued: 1,
                created_at: test_date() + chrono::Duration::minutes(minutes),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let res = s.server.get(URL).add_header("authorization", &s.auth).await;

    res.assert_status_ok();
    let repo = res.json::<Value>()["message"].clone();
    let deliveries = repo["recent_deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 1, "{repo}");
    assert_eq!(
        deliveries[0]["repository"],
        "https://gitea.example.com/acme/app"
    );
    let events = &repo["chain"]["incoming_events"];
    assert_eq!(events["state"], "ok", "{events}");
    assert_eq!(events["detail"], "push event queued an evaluation");
    assert_eq!(events["action"], Value::Null);
    assert_eq!(repo["chain"]["commit_status"]["state"], "off");
    s.db.finish().await;
}

#[tokio::test]
async fn patch_task_repository_refuses_git_host_connection_when_host_differs_from_task_repository_host()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let gitlab = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitlab.example.com",
            false,
        ),
    )
    .await;

    let res = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": gitlab.id, "report_status": false }))
        .await;

    res.assert_status_bad_request();
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("differ") && message.contains("gitlab.example.com"),
        "{message}"
    );
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );
    s.db.finish().await;
}

#[tokio::test]
async fn patch_task_repository_refuses_git_host_connection_belonging_to_another_project() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let other = gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        rows::project_named("other-project"),
    )
    .await
    .unwrap();
    let foreign = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            other.id,
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    let res = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": foreign.id, "report_status": false }))
        .await;

    res.assert_status_bad_request();
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("can't use"), "{message}");
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );
    s.db.finish().await;
}

#[tokio::test]
async fn project_member_with_edit_task_permission_but_not_manage_integrations_can_select_git_host_connection_for_task()
 {
    let task_editor = role::Model {
        id: RoleId::new(Uuid::from_u128(0xed17)),
        name: "task-editor".into(),
        project: Some(project_id()),
        permission: mask_from(&[Permission::ViewProject, Permission::EditTask]),
        managed: false,
    };
    let s = create_test_database_with_project_membership_for_role(task_editor).await;
    let connection = insert_test_git_host_connection(
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
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.id, "report_status": true }))
        .await;

    res.assert_status_ok();
    let repo = res.json::<Value>()["message"].clone();
    assert_eq!(repo["can_manage_integrations"], false);
    assert_eq!(repo["git_host_connection"]["can_manage"], false);
    assert!(
        repo["git_host_connection"]["webhook_url"].is_string(),
        "{repo}"
    );
    let task = read_test_task_row_from_database(&s).await;
    assert_eq!(task.git_host_connection, Some(connection.id));
    assert!(task.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn enabling_commit_status_reports_with_connection_that_cannot_report_status_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let events_only = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            false,
        ),
    )
    .await;

    let refused = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": events_only.id, "report_status": true }))
        .await;
    let without_connection = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": null, "report_status": true }))
        .await;

    refused.assert_status_bad_request();
    without_connection.assert_status_bad_request();
    let task = read_test_task_row_from_database(&s).await;
    assert_eq!(task.git_host_connection, None);
    assert!(!task.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn view_only_project_members_cannot_change_task_repository_connection() {
    let s = create_test_database_with_project_membership_for_role(rows::view_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;

    let patch = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.id, "report_status": true }))
        .await;
    let install = s
        .server
        .post(&format!("{URL}/install-webhook"))
        .add_header("authorization", &s.auth)
        .await;
    let view = s.server.get(URL).add_header("authorization", &s.auth).await;

    patch.assert_status_forbidden();
    install.assert_status_forbidden();
    view.assert_status_ok();
    let repo = view.json::<Value>()["message"].clone();
    assert_eq!(repo["can_edit"], false);
    assert_eq!(repo["can_manage_integrations"], false);
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );
    s.db.finish().await;
}

#[tokio::test]
async fn install_deploy_key_fails_when_no_git_host_connection_is_selected() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;

    let res = s
        .server
        .post(&format!("{URL}/install-deploy-key"))
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_bad_request();
    s.db.finish().await;
}

#[tokio::test]
async fn installing_a_deploy_key_refuses_a_git_host_in_a_private_network() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let internal = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "http://10.0.0.5:8080",
            true,
        ),
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET repository = 'http://10.0.0.5:8080/acme/app.git', git_host_connection = '{}' WHERE id = '{}'",
        internal.id,
        task_id()
    ))
    .await;

    let res = s
        .server
        .post(&format!("{URL}/install-deploy-key"))
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("10.0.0.5"),
        "{body}"
    );
    s.db.finish().await;
}

#[tokio::test]
async fn commit_status_test_returns_conflict_when_task_has_no_evaluated_commit() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    let status_test = format!("{URL}/status-test");

    let without_connection = s
        .server
        .post(&status_test)
        .add_header("authorization", &s.auth)
        .await;
    s.server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.id, "report_status": true }))
        .await
        .assert_status_ok();
    let without_evaluation = s
        .server
        .post(&status_test)
        .add_header("authorization", &s.auth)
        .await;

    without_connection.assert_status(StatusCode::CONFLICT);
    without_evaluation.assert_status(StatusCode::CONFLICT);
    assert!(
        without_evaluation.json::<Value>()["message"]
            .as_str()
            .unwrap()
            .contains("no evaluated commit"),
    );
    s.db.finish().await;
}

async fn create_task_and_read_its_row(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    extra: Value,
) -> (StatusCode, Option<task::Model>) {
    let mut body = json!({
        "name": "new-task",
        "display_name": "New Task",
        "description": "",
        "repository": "https://gitea.example.com/acme/other",
        "wildcard": "packages",
        "triggers": [],
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let res = s
        .server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&body)
        .await;
    let row =
        s.db.rows::<task::Entity>()
            .await
            .into_iter()
            .find(|task| task.name == "new-task");
    (res.status_code(), row)
}

#[tokio::test]
async fn a_new_task_gets_the_only_fitting_connection_with_its_status_reporting() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let fitting = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://codeberg.org",
            true,
        ),
    )
    .await;

    let (status, row) = create_task_and_read_its_row(&s, json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let row = row.unwrap();
    assert_eq!(row.git_host_connection, Some(fitting.id));
    assert!(row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn a_new_task_gets_no_connection_when_several_fit() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    for _ in 0..2 {
        insert_test_git_host_connection(
            &s,
            build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                true,
            ),
        )
        .await;
    }

    let (status, row) = create_task_and_read_its_row(&s, json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let row = row.unwrap();
    assert_eq!(row.git_host_connection, None);
    assert!(!row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn a_new_task_takes_the_chosen_connection_and_refuses_one_that_does_not_fit() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let chosen = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    let other_host = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://codeberg.org",
            true,
        ),
    )
    .await;

    let (status, row) =
        create_task_and_read_its_row(&s, json!({ "git_host_connection": other_host.id })).await;
    assert!(status.is_client_error(), "{status}");
    assert!(row.is_none());

    let (status, row) = create_task_and_read_its_row(
        &s,
        json!({ "git_host_connection": chosen.id, "report_status": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = row.unwrap();
    assert_eq!(row.git_host_connection, Some(chosen.id));
    assert!(!row.report_status);
    s.db.finish().await;
}

async fn database_with_task_using_a_status_reporting_connection() -> (
    RealTestDatabaseHttpServerAndBearerAuthHeader,
    git_host_connection::Model,
) {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let connection = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    s.server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection.id, "report_status": true }))
        .await
        .assert_status_ok();
    (s, connection)
}

async fn patch_task_repository(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    body: Value,
) -> task::Model {
    s.server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&body)
        .await
        .assert_status_ok();
    read_test_task_row_from_database(s).await
}

#[tokio::test]
async fn turning_commit_status_off_and_on_keeps_the_connection() {
    let (s, connection) = database_with_task_using_a_status_reporting_connection().await;

    let off = patch_task_repository(&s, json!({ "report_status": false })).await;
    let on = patch_task_repository(&s, json!({ "report_status": true })).await;

    assert_eq!(off.git_host_connection, Some(connection.id));
    assert!(!off.report_status);
    assert_eq!(on.git_host_connection, Some(connection.id));
    assert!(on.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn choosing_another_connection_keeps_commit_status_on() {
    let (s, _) = database_with_task_using_a_status_reporting_connection().await;
    let other = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;

    let row = patch_task_repository(&s, json!({ "git_host_connection": other.id })).await;

    assert_eq!(row.git_host_connection, Some(other.id));
    assert!(row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn an_explicit_null_connection_removes_the_connection_and_its_commit_status() {
    let (s, _) = database_with_task_using_a_status_reporting_connection().await;

    let row = patch_task_repository(&s, json!({ "git_host_connection": null })).await;

    assert_eq!(row.git_host_connection, None);
    assert!(!row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn an_empty_patch_changes_nothing() {
    let (s, connection) = database_with_task_using_a_status_reporting_connection().await;

    let row = patch_task_repository(&s, json!({})).await;

    assert_eq!(row.git_host_connection, Some(connection.id));
    assert!(row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn a_task_of_declarative_state_shows_its_repository_read_only() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let declared = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET managed = true, git_host_connection = '{}'",
        declared.id
    ))
    .await;

    let view = s.server.get(URL).add_header("authorization", &s.auth).await;
    let patch = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "report_status": true }))
        .await;
    let status_test = s
        .server
        .post(&format!("{URL}/status-test"))
        .add_header("authorization", &s.auth)
        .await;
    view.assert_status_ok();
    let repo = view.json::<Value>()["message"].clone();
    assert_eq!(repo["can_edit"], false);
    assert_eq!(repo["chain"]["connection"]["action"], Value::Null, "{repo}");
    assert_eq!(
        repo["chain"]["commit_status"]["action"],
        Value::Null,
        "{repo}"
    );
    patch.assert_status_forbidden();
    status_test.assert_status_forbidden();
    assert!(!read_test_task_row_from_database(&s).await.report_status);
    s.db.finish().await;
}

async fn read_task_repository_without_connection_after(statement: &str) -> Value {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    if !statement.is_empty() {
        s.db.execute(statement).await;
    }
    let res = s.server.get(URL).add_header("authorization", &s.auth).await;
    res.assert_status_ok();
    let repo = res.json::<Value>()["message"].clone();
    s.db.finish().await;
    repo
}

#[tokio::test]
async fn only_tasks_and_projects_outside_declarative_state_offer_to_create_a_connection() {
    let editable = read_task_repository_without_connection_after("").await;
    assert_eq!(editable["can_create_connection"], true);
    assert_eq!(
        editable["chain"]["connection"]["action"]["type"], "create_connection",
        "{editable}"
    );

    for statement in [
        "UPDATE task SET managed = true",
        "UPDATE project SET managed = true",
    ] {
        let declared = read_task_repository_without_connection_after(statement).await;
        assert_eq!(declared["can_create_connection"], false, "{statement}");
        assert_eq!(
            declared["chain"]["connection"]["action"],
            Value::Null,
            "{statement}: {declared}"
        );
    }
}

const GIT_HOST_SECRET_MARKER: &str = "SECRET-GIT-HOST-BODY";

async fn test_task_with_evaluated_commit_and_gitea_status_host_calls_status_test_endpoint(
    base_url: &str,
    allowed_non_public_network_targets: Vec<String>,
) -> String {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let crypt = crypt.to_string_lossy().into_owned();
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            access_token: Some(gradient_sources::encrypt_secret(&crypt, "token").unwrap()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                base_url,
                true,
            )
        },
    )
    .await;
    let conn = s.db.connection();
    let commit =
        gradient_db::rows::insert::<commit::Entity, _>(&conn, rows::build_request_commit())
            .await
            .unwrap();
    let evaluation = gradient_db::rows::insert::<evaluation::Entity, _>(
        &conn,
        rows::queued_evaluation(task_id(), commit.id),
    )
    .await
    .unwrap();
    s.db.execute(&format!(
        "UPDATE task SET repository = '{base_url}/acme/app.git', git_host_connection = '{}', last_evaluation = '{}' WHERE id = '{}'",
        connection.id,
        evaluation.id,
        task_id()
    ))
    .await;
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = allowed_non_public_network_targets;
        })
        .server();

    let res = server
        .post(&format!("{URL}/status-test"))
        .add_header("authorization", &s.auth)
        .await;
    res.assert_status_ok();
    let body = res.text();
    s.db.finish().await;
    body
}

#[tokio::test]
async fn commit_status_test_endpoint_does_not_connect_to_git_host_connection_when_allow_list_is_empty()
 {
    let (base_url, connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");

    let body = test_task_with_evaluated_commit_and_gitea_status_host_calls_status_test_endpoint(
        &base_url,
        Vec::new(),
    )
    .await;

    assert_eq!(connections.load(Ordering::SeqCst), 0, "{body}");
    assert!(!body.contains(GIT_HOST_SECRET_MARKER), "{body}");
}

#[tokio::test]
async fn control_case_commit_status_endpoint_connects_to_git_host_at_localhost_when_admin_allow_list_contains_localhost()
 {
    let (base_url, connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");

    let body = test_task_with_evaluated_commit_and_gitea_status_host_calls_status_test_endpoint(
        &base_url,
        vec!["localhost".into()],
    )
    .await;

    assert!(connections.load(Ordering::SeqCst) >= 1, "{body}");
}

async fn store_gitea_connection_without_base_url_call_task_install_endpoint_and_count_git_host_requests(
    endpoint: &str,
) -> (axum_test::TestResponse, usize) {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let (git_host_url, connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let git_host_url = git_host_url.replace("127.0.0.1", "localhost");
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let crypt = crypt.to_string_lossy().into_owned();
    s.db.execute(
        "ALTER TABLE git_host_connection DROP CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url",
    )
    .await;
    let connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            base_url: None,
            secret: Some(gradient_sources::encrypt_secret(&crypt, "secret").unwrap()),
            access_token: Some(gradient_sources::encrypt_secret(&crypt, "token").unwrap()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                &git_host_url,
                true,
            )
        },
    )
    .await;
    s.db.execute(
        "ALTER TABLE git_host_connection ADD CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url \
         CHECK (git_host_type NOT IN (0, 1, 2) OR base_url IS NOT NULL) NOT VALID",
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET repository = '{git_host_url}/acme/app.git', git_host_connection = '{}' WHERE id = '{}'",
        connection.id,
        task_id()
    ))
    .await;
    let conn = s.db.connection();
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = vec!["localhost".into()];
        })
        .server();

    let res = server
        .post(&format!("{URL}/{endpoint}"))
        .add_header("authorization", &s.auth)
        .await;

    s.db.finish().await;
    (res, connections.load(Ordering::SeqCst))
}

#[tokio::test]
async fn gitea_connection_without_base_url_refuses_webhook_or_deploy_key_install_without_contacting_git_host()
 {
    for endpoint in ["install-webhook", "install-deploy-key"] {
        let (res, connections) =
            store_gitea_connection_without_base_url_call_task_install_endpoint_and_count_git_host_requests(endpoint).await;

        assert!(
            res.status_code().is_client_error(),
            "{endpoint}: {}",
            res.text()
        );
        assert!(
            res.text().contains("base URL"),
            "{endpoint}: {}",
            res.text()
        );
        assert_eq!(connections, 0, "{endpoint}");
    }
}

const NO_TASK_REPOSITORY_GRANT_REFUSAL_TEXT: &str = "has no grant for this repository";

fn build_instance_owned_gitea_connection_with_base_url(
    base_url: &str,
) -> git_host_connection::Model {
    git_host_connection::Model {
        project: None,
        ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            project_id(),
            base_url,
            false,
        )
    }
}

async fn insert_project_git_host_repository_grant(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
    project: ProjectId,
    repository_name: Option<&str>,
) {
    gradient_db::git_host_connection_repository_grants::insert(
        &s.db.connection(),
        NewGitHostConnectionRepositoryGrant {
            connection,
            project,
            repository_owner: "acme",
            repository_name,
            source: RepositoryGrantSource::Admin,
            created_by: Some(user_id()),
        },
    )
    .await
    .unwrap();
}

async fn send_task_update_request_to_attach_git_host_connection(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    connection: GitHostConnectionId,
) -> axum_test::TestResponse {
    s.server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": connection, "report_status": false }))
        .await
}

async fn fetch_available_git_host_connection_ids_for_test_task(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> Vec<String> {
    let res = s.server.get(URL).add_header("authorization", &s.auth).await;
    res.assert_status_ok();
    res.json::<Value>()["message"]["available_connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_response_is_forbidden_for_missing_repository_grant(res: &axum_test::TestResponse) {
    res.assert_status(StatusCode::FORBIDDEN);
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains(NO_TASK_REPOSITORY_GRANT_REFUSAL_TEXT),
        "{message}"
    );
}

#[tokio::test]
async fn attach_instance_git_host_connection_without_repository_grant_is_refused_and_stores_nothing()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com"),
    )
    .await;

    assert_response_is_forbidden_for_missing_repository_grant(
        &send_task_update_request_to_attach_git_host_connection(&s, instance.id).await,
    );
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );
    assert!(
        !fetch_available_git_host_connection_ids_for_test_task(&s)
            .await
            .contains(&instance.id.to_string())
    );
    s.db.finish().await;
}

#[tokio::test]
async fn attaching_instance_owned_git_host_connection_to_task_with_project_grant_for_repository_is_allowed()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com"),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;

    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        Some(instance.id)
    );
    assert!(
        fetch_available_git_host_connection_ids_for_test_task(&s)
            .await
            .contains(&instance.id.to_string())
    );
    s.db.finish().await;
}

#[tokio::test]
async fn attaching_instance_git_host_connection_with_whole_repository_owner_grant_is_allowed() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com"),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), None).await;

    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        Some(instance.id)
    );
    s.db.finish().await;
}

#[tokio::test]
async fn repository_grant_for_another_project_does_not_allow_attaching_git_host_connection_to_task()
{
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let other = gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        rows::project_named("other-project"),
    )
    .await
    .unwrap();
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com"),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, other.id, Some("app")).await;

    assert_response_is_forbidden_for_missing_repository_grant(
        &send_task_update_request_to_attach_git_host_connection(&s, instance.id).await,
    );
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );

    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();
    s.db.finish().await;
}

#[tokio::test]
async fn attaching_team_git_host_connection_to_task_of_project_not_owned_by_team_is_refused() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    gradient_db::rows::insert::<team::Entity, _>(&s.db.connection(), rows::team())
        .await
        .unwrap();
    let team_connection = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            owner_team: Some(team_id()),
            ..build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com")
        },
    )
    .await;
    insert_project_git_host_repository_grant(&s, team_connection.id, project_id(), Some("app"))
        .await;

    let res = send_task_update_request_to_attach_git_host_connection(&s, team_connection.id).await;

    res.assert_status_bad_request();
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("isn't available in the task's project"),
        "{message}"
    );
    assert_eq!(
        read_test_task_row_from_database(&s)
            .await
            .git_host_connection,
        None
    );
    s.db.finish().await;
}

#[tokio::test]
async fn install_deploy_key_and_webhook_on_task_repository_sends_no_request_to_git_host_after_repository_access_removed()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let crypt = crypt.to_string_lossy().into_owned();
    let instance = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            access_token: Some(gradient_sources::encrypt_secret(&crypt, "token").unwrap()),
            secret: Some(gradient_sources::encrypt_secret(&crypt, "webhook-secret").unwrap()),
            ..build_instance_owned_gitea_connection_with_base_url(&base_url)
        },
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET repository = '{base_url}/acme/app.git' WHERE id = '{}'",
        task_id()
    ))
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();
    s.db.execute("DELETE FROM git_host_connection_repository_grant")
        .await;
    let conn = s.db.connection();
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = vec!["localhost".into()];
        })
        .server();

    for path in ["install-deploy-key", "install-webhook"] {
        let res = server
            .post(&format!("{URL}/{path}"))
            .add_header("authorization", &s.auth)
            .await;
        assert_response_is_forbidden_for_missing_repository_grant(&res);
    }
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
    s.db.finish().await;
}

#[tokio::test]
async fn install_webhook_or_deploy_key_with_oauth_refresh_token_on_other_host_returns_422_with_refusal_text_and_sends_no_request()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let crypt = crypt.to_string_lossy().into_owned();
    let instance = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            access_token: Some(gradient_sources::encrypt_secret(&crypt, "token").unwrap()),
            encrypted_refresh_token: Some(
                gradient_sources::encrypt_secret(&crypt, "refresh").unwrap(),
            ),
            secret: Some(gradient_sources::encrypt_secret(&crypt, "webhook-secret").unwrap()),
            ..build_instance_owned_gitea_connection_with_base_url(&base_url)
        },
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET repository = '{base_url}/acme/app.git' WHERE id = '{}'",
        task_id()
    ))
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();
    let conn = s.db.connection();
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = vec!["localhost".into()];
            cli.gitea_app.instance_base_url = Some("https://codeberg.example/".into());
            cli.gitea_app.client_id = Some("gradient-client".into());
            cli.gitea_app.client_secret_file = Some("/run/secrets/gitea-app".into());
        })
        .server();

    for path in ["install-deploy-key", "install-webhook"] {
        let res = server
            .post(&format!("{URL}/{path}"))
            .add_header("authorization", &s.auth)
            .await;
        res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
        assert!(message.contains(&instance.id.to_string()), "{message}");
        assert!(message.contains("https://codeberg.example"), "{message}");
        assert!(message.contains(&base_url), "{message}");
    }
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
    s.db.finish().await;
}

const TEST_TASK_REST_PATH: &str = "/api/v1/tasks/test-project/test-task";
const GITEA_CONNECTION_NO_BASE_URL_MESSAGE_PART: &str = "has no base URL";

fn create_temporary_encryption_key_file() -> String {
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    crypt.to_string_lossy().into_owned()
}

fn build_instance_gitea_connection_row_with_encrypted_token_webhook_secret_and_optional_base_url(
    crypt: &str,
    base_url: Option<&str>,
) -> git_host_connection::Model {
    git_host_connection::Model {
        access_token: Some(gradient_sources::encrypt_secret(crypt, "token").unwrap()),
        secret: Some(gradient_sources::encrypt_secret(crypt, "webhook-secret").unwrap()),
        base_url: base_url.map(str::to_owned),
        ..build_instance_owned_gitea_connection_with_base_url("https://unused.example")
    }
}

fn start_test_http_server_with_crypt_file_allowing_outgoing_localhost_requests(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    crypt: String,
) -> TestServer {
    let conn = s.db.connection();
    TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = vec!["localhost".into()];
        })
        .server()
}

async fn set_test_task_repository_and_git_host_connection_in_database(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    repository: &str,
    connection: GitHostConnectionId,
) {
    s.db.execute(&format!(
        "UPDATE task SET repository = '{repository}', git_host_connection = '{connection}' WHERE id = '{}'",
        task_id()
    ))
    .await;
}

async fn send_patch_request_changing_task_repository_url(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
    repository: &str,
) -> axum_test::TestResponse {
    s.server
        .patch(TEST_TASK_REST_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "repository": repository }))
        .await
}

async fn send_test_task_deploy_key_and_webhook_install_requests(
    server: &TestServer,
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> Vec<axum_test::TestResponse> {
    let mut responses = Vec::new();
    for path in ["install-deploy-key", "install-webhook"] {
        responses.push(
            server
                .post(&format!("{URL}/{path}"))
                .add_header("authorization", &s.auth)
                .await,
        );
    }
    responses
}

fn assert_bad_request_for_connection_without_base_url(res: &axum_test::TestResponse) {
    res.assert_status_bad_request();
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains(GITEA_CONNECTION_NO_BASE_URL_MESSAGE_PART),
        "{message}"
    );
}

#[tokio::test]
async fn gitea_connection_without_base_url_is_refused_and_no_access_token_is_sent_to_task_repository_host()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = create_temporary_encryption_key_file();
    s.db.execute(
        "ALTER TABLE git_host_connection DROP CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url",
    )
    .await;
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_gitea_connection_row_with_encrypted_token_webhook_secret_and_optional_base_url(&crypt, None),
    )
    .await;
    s.db.execute(
        "ALTER TABLE git_host_connection ADD CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url \
         CHECK (git_host_type NOT IN (0, 1, 2) OR base_url IS NOT NULL) NOT VALID",
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), None).await;
    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        "git@gitea.example.com:acme/app.git",
        instance.id,
    )
    .await;

    assert_bad_request_for_connection_without_base_url(
        &send_patch_request_changing_task_repository_url(&s, "https://attacker.example/acme/infra")
            .await,
    );
    assert_eq!(
        read_test_task_row_from_database(&s).await.repository,
        "git@gitea.example.com:acme/app.git"
    );

    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        &format!("{base_url}/acme/app.git"),
        instance.id,
    )
    .await;
    let server =
        start_test_http_server_with_crypt_file_allowing_outgoing_localhost_requests(&s, crypt);
    for res in send_test_task_deploy_key_and_webhook_install_requests(&server, &s).await {
        assert_bad_request_for_connection_without_base_url(&res);
    }
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
    s.db.finish().await;
}

#[tokio::test]
async fn changing_task_repository_to_repository_without_grant_with_instance_connection_is_refused_and_stores_nothing()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_owned_gitea_connection_with_base_url("https://gitea.example.com"),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    send_task_update_request_to_attach_git_host_connection(&s, instance.id)
        .await
        .assert_status_ok();

    assert_response_is_forbidden_for_missing_repository_grant(
        &send_patch_request_changing_task_repository_url(
            &s,
            "git@gitea.example.com:acme/other.git",
        )
        .await,
    );
    assert_eq!(
        read_test_task_row_from_database(&s).await.repository,
        "git@gitea.example.com:acme/app.git"
    );
    s.db.finish().await;
}

#[tokio::test]
async fn deploy_key_and_webhook_install_sends_no_request_when_task_repository_host_differs_from_connection_base_url()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let connection_base_url = base_url.replace("127.0.0.1", "localhost");
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = create_temporary_encryption_key_file();
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_gitea_connection_row_with_encrypted_token_webhook_secret_and_optional_base_url(
            &crypt,
            Some(&connection_base_url),
        ),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        &format!("{base_url}/acme/app.git"),
        instance.id,
    )
    .await;

    let server =
        start_test_http_server_with_crypt_file_allowing_outgoing_localhost_requests(&s, crypt);
    for res in send_test_task_deploy_key_and_webhook_install_requests(&server, &s).await {
        res.assert_status_bad_request();
        let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
        assert!(message.contains("differ"), "{message}");
    }
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
    s.db.finish().await;
}

#[tokio::test]
async fn an_instance_gitlab_connection_without_base_url_is_accepted_for_a_gitlab_com_task_repository_and_refused_for_another_host_and_an_instance_gitea_connection_without_base_url_is_refused()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    s.db.execute(
        "ALTER TABLE git_host_connection DROP CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url",
    )
    .await;
    let legacy_gitlab = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            git_host_type: GitHostType::GitLab,
            base_url: None,
            ..build_instance_owned_gitea_connection_with_base_url("https://unused.example")
        },
    )
    .await;
    let legacy_gitea = insert_test_git_host_connection(
        &s,
        git_host_connection::Model {
            base_url: None,
            ..build_instance_owned_gitea_connection_with_base_url("https://unused.example")
        },
    )
    .await;
    s.db.execute(
        "ALTER TABLE git_host_connection ADD CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url \
         CHECK (git_host_type NOT IN (0, 1, 2) OR base_url IS NOT NULL) NOT VALID",
    )
    .await;
    for connection in [legacy_gitlab.id, legacy_gitea.id] {
        insert_project_git_host_repository_grant(&s, connection, project_id(), Some("app")).await;
    }

    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        "https://gitlab.com/acme/app",
        legacy_gitlab.id,
    )
    .await;
    send_task_update_request_to_attach_git_host_connection(&s, legacy_gitlab.id)
        .await
        .assert_status_ok();
    assert_bad_request_for_connection_without_base_url(
        &send_task_update_request_to_attach_git_host_connection(&s, legacy_gitea.id).await,
    );

    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        "https://gitlab.example.com/acme/app",
        legacy_gitlab.id,
    )
    .await;
    let res = send_task_update_request_to_attach_git_host_connection(&s, legacy_gitlab.id).await;
    res.assert_status_bad_request();
    let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("differ") && message.contains("gitlab.com"),
        "{message}"
    );
    s.db.finish().await;
}

#[tokio::test]
async fn attach_connection_to_task_is_refused_when_stored_base_url_has_query_and_no_deploy_key_or_webhook_install_request_is_sent()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter(GIT_HOST_SECRET_MARKER);
    let base_url = base_url.replace("127.0.0.1", "localhost");
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let crypt = create_temporary_encryption_key_file();
    let instance = insert_test_git_host_connection(
        &s,
        build_instance_gitea_connection_row_with_encrypted_token_webhook_secret_and_optional_base_url(
            &crypt,
            Some(&format!("{base_url}?private_token=leak")),
        ),
    )
    .await;
    insert_project_git_host_repository_grant(&s, instance.id, project_id(), Some("app")).await;
    set_test_task_repository_and_git_host_connection_in_database(
        &s,
        &format!("{base_url}/acme/app.git"),
        instance.id,
    )
    .await;

    let server =
        start_test_http_server_with_crypt_file_allowing_outgoing_localhost_requests(&s, crypt);
    let attach = server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "git_host_connection": instance.id, "report_status": false }))
        .await;
    for res in std::iter::once(attach)
        .chain(send_test_task_deploy_key_and_webhook_install_requests(&server, &s).await)
    {
        res.assert_status_bad_request();
        let message = res.json::<Value>()["message"].as_str().unwrap().to_owned();
        assert!(message.contains("query"), "{message}");
    }
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
    s.db.finish().await;
}

async fn insert_team_owned_gitea_connection_and_make_test_project_owned_by_team(
    s: &RealTestDatabaseHttpServerAndBearerAuthHeader,
) -> git_host_connection::Model {
    gradient_db::rows::insert::<team::Entity, _>(&s.db.connection(), rows::team())
        .await
        .unwrap();
    s.db.execute(&format!(
        "UPDATE project SET owner_team = '{}' WHERE id = '{}'",
        team_id(),
        project_id()
    ))
    .await;
    insert_test_git_host_connection(
        s,
        git_host_connection::Model {
            project: None,
            owner_team: Some(team_id()),
            ..build_gitea_git_host_connection_for_project_with_event_and_status_flags(
                project_id(),
                "https://gitea.example.com",
                true,
            )
        },
    )
    .await
}

#[tokio::test]
async fn create_task_without_connection_when_single_team_connection_matches_host_and_project_has_no_repository_grant()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    insert_team_owned_gitea_connection_and_make_test_project_owned_by_team(&s).await;

    let (status, row) = create_task_and_read_its_row(&s, json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let row = row.unwrap();
    assert_eq!(row.git_host_connection, None);
    assert!(!row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn reject_task_that_names_team_connection_without_repository_grant_and_store_no_rows() {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let team_connection =
        insert_team_owned_gitea_connection_and_make_test_project_owned_by_team(&s).await;

    let res = s
        .server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&json!({
            "name": "new-task",
            "display_name": "New Task",
            "description": "",
            "repository": "https://gitea.example.com/acme/other",
            "wildcard": "packages",
            "triggers": [],
            "git_host_connection": team_connection.id,
        }))
        .await;

    assert_response_is_forbidden_for_missing_repository_grant(&res);
    assert!(
        s.db.rows::<task::Entity>()
            .await
            .iter()
            .all(|task| task.name != "new-task")
    );
    assert!(
        s.db.rows::<gradient_entity::git_host_connection_repository_grant::Entity>()
            .await
            .is_empty()
    );
    s.db.finish().await;
}

#[tokio::test]
async fn create_task_without_connection_picks_only_matching_team_connection_when_project_has_repository_grant()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let team_connection =
        insert_team_owned_gitea_connection_and_make_test_project_owned_by_team(&s).await;
    insert_project_git_host_repository_grant(&s, team_connection.id, project_id(), Some("other"))
        .await;

    let (status, row) = create_task_and_read_its_row(&s, json!({})).await;

    assert_eq!(status, StatusCode::OK);
    let row = row.unwrap();
    assert_eq!(row.git_host_connection, Some(team_connection.id));
    assert!(row.report_status);
    s.db.finish().await;
}

async fn create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
-> (
    RealTestDatabaseHttpServerAndBearerAuthHeader,
    git_host_connection::Model,
) {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let team_connection =
        insert_team_owned_gitea_connection_and_make_test_project_owned_by_team(&s).await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}', report_status = false WHERE id = '{}'",
        team_connection.id,
        task_id()
    ))
    .await;
    (s, team_connection)
}

#[tokio::test]
async fn empty_task_repository_patch_keeps_team_connection_without_repository_grant() {
    let (s, team_connection) =
        create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
            .await;

    let row = patch_task_repository(&s, json!({})).await;

    assert_eq!(row.git_host_connection, Some(team_connection.id));
    assert!(!row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn task_repository_patches_omitting_git_host_connection_keep_team_link_without_repository_grant_and_report_status_off()
 {
    let (s, team_connection) =
        create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
            .await;

    for body in [json!({ "report_status": false }), json!({})] {
        let row = patch_task_repository(&s, body.clone()).await;

        assert_eq!(row.git_host_connection, Some(team_connection.id), "{body}");
        assert!(!row.report_status, "{body}");
    }
    s.db.finish().await;
}

#[tokio::test]
async fn refuses_commit_status_for_team_link_without_repository_grant() {
    let (s, team_connection) =
        create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
            .await;

    let res = s
        .server
        .patch(URL)
        .add_header("authorization", &s.auth)
        .json(&json!({ "report_status": true }))
        .await;

    assert_response_is_forbidden_for_missing_repository_grant(&res);
    let row = read_test_task_row_from_database(&s).await;
    assert_eq!(row.git_host_connection, Some(team_connection.id));
    assert!(!row.report_status);
    s.db.finish().await;
}

#[tokio::test]
async fn changing_task_repository_url_to_another_repository_without_grant_on_team_connection_is_refused_and_keeps_link_and_url()
 {
    let (s, team_connection) =
        create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
            .await;

    assert_response_is_forbidden_for_missing_repository_grant(
        &send_patch_request_changing_task_repository_url(
            &s,
            "https://gitea.example.com/acme/infra",
        )
        .await,
    );
    let row = read_test_task_row_from_database(&s).await;
    assert_eq!(row.git_host_connection, Some(team_connection.id));
    assert_eq!(row.repository, "git@gitea.example.com:acme/app.git");
    s.db.finish().await;
}

#[tokio::test]
async fn rename_task_linked_to_team_connection_without_repository_grant_succeeds_and_keeps_link() {
    let (s, team_connection) =
        create_post_upgrade_task_linked_to_team_gitea_connection_without_repository_grant_and_commit_status_off()
            .await;

    s.server
        .patch(TEST_TASK_REST_PATH)
        .add_header("authorization", &s.auth)
        .json(&json!({ "name": "renamed-task" }))
        .await
        .assert_status_ok();

    let row = read_test_task_row_from_database(&s).await;
    assert_eq!(row.name, "renamed-task");
    assert_eq!(row.git_host_connection, Some(team_connection.id));
    s.db.finish().await;
}

#[tokio::test]
async fn empty_patch_of_task_repository_settings_removes_link_to_connection_owned_by_another_project_and_turns_off_commit_status_reporting()
 {
    let s = create_test_database_with_project_membership_for_role(rows::admin_role()).await;
    let other = gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        rows::project_named("other-project"),
    )
    .await
    .unwrap();
    let foreign = insert_test_git_host_connection(
        &s,
        build_gitea_git_host_connection_for_project_with_event_and_status_flags(
            other.id,
            "https://gitea.example.com",
            true,
        ),
    )
    .await;
    s.db.execute(&format!(
        "UPDATE task SET git_host_connection = '{}', report_status = true WHERE id = '{}'",
        foreign.id,
        task_id()
    ))
    .await;

    let row = patch_task_repository(&s, json!({})).await;

    assert_eq!(row.git_host_connection, None);
    assert!(!row.report_status);
    s.db.finish().await;
}
