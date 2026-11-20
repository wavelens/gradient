/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::{git_host_connection, project, role, task};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
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
async fn task_repository_chain_uses_newest_delivery_for_the_task_repository_only() {
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
        message.contains("does not match") && message.contains("gitlab.example.com"),
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
