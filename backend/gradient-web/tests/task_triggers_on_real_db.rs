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
use gradient_entity::{git_host_connection, task, task_action, task_trigger};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use gradient_types::ids::*;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::{GitHostType, RoleId};
use serde_json::{Value, json};
use uuid::Uuid;

struct Seeded {
    db: TestDatabase,
    server: TestServer,
    auth: String,
}

async fn seeded(role: RoleId) -> Seeded {
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
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(&conn, rows::project())
        .await
        .unwrap();
    for role in [rows::admin_role(), rows::view_role()] {
        gradient_db::rows::insert::<gradient_entity::role::Entity, _>(&conn, role)
            .await
            .unwrap();
    }
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        rows::project_member(role),
    )
    .await
    .unwrap();

    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let server = TestState::new()
        .web_db(conn)
        .cli(|cli| cli.secrets.crypt_file = crypt.to_string_lossy().into_owned())
        .server();
    Seeded {
        db,
        server,
        auth: bearer(session),
    }
}

fn new_task(triggers: Option<Value>) -> Value {
    let mut body = json!({
        "name": "app",
        "display_name": "App",
        "description": "",
        "repository": "git@gitea.example.com:acme/app.git",
        "wildcard": "packages.x86_64-linux.**",
        "exec_enabled": true,
    });
    if let Some(triggers) = triggers {
        body["triggers"] = triggers;
    }
    body
}

fn test_project_gitea_git_host_connection_for_webhook_events() -> git_host_connection::Model {
    git_host_connection::Model {
        id: GitHostConnectionId::now_v7(),
        project: Some(project_id()),
        name: "gitea".into(),
        display_name: "Gitea".into(),
        git_host_type: GitHostType::Gitea,
        base_url: Some("https://gitea.example.com".into()),
        secret: Some("encrypted".into()),
        receives_events: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn configs(rows: &[task_trigger::Model]) -> Vec<(TriggerConfig, bool)> {
    let mut rows = rows.to_vec();
    rows.sort_by_key(|t| t.id);
    rows.iter()
        .map(|t| {
            (
                TriggerConfig::parse_row(t.trigger_type, &t.config).unwrap(),
                t.is_default,
            )
        })
        .collect()
}

#[tokio::test]
async fn creating_a_task_with_triggers_stores_exactly_those_triggers() {
    let s = seeded(BASE_ROLE_ADMIN_ID).await;
    let triggers = json!([
        { "type": "polling", "interval_secs": 120, "branch": "release" },
        { "type": "time", "cron": "0 3 * * *", "branch": "nightly" },
    ]);

    let res = s
        .server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&new_task(Some(triggers)))
        .await;

    res.assert_status_ok();
    let tasks = s.db.rows::<task::Entity>().await;
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].exec_enabled);
    assert_eq!(
        configs(&s.db.rows::<task_trigger::Entity>().await),
        vec![
            (
                TriggerConfig::Polling {
                    interval_secs: 120,
                    branch: Some("release".into()),
                },
                true
            ),
            (
                TriggerConfig::Time {
                    cron: "0 0 3 * * *".into(),
                    branch: Some("nightly".into()),
                },
                false
            ),
        ]
    );
    s.db.finish().await;
}

#[tokio::test]
async fn creating_a_task_without_triggers_adds_one_default_polling_trigger() {
    let s = seeded(BASE_ROLE_ADMIN_ID).await;

    s.server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&new_task(None))
        .await
        .assert_status_ok();
    s.server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&{
            let mut body = new_task(Some(json!([])));
            body["name"] = json!("bare");
            body
        })
        .await
        .assert_status_ok();

    let rows = s.db.rows::<task_trigger::Entity>().await;
    assert_eq!(
        configs(&rows),
        vec![(
            TriggerConfig::Polling {
                interval_secs: 300,
                branch: None,
            },
            true
        )]
    );
    s.db.finish().await;
}

#[tokio::test]
async fn connecting_a_git_host_adds_no_triggers_to_existing_tasks() {
    let s = seeded(BASE_ROLE_ADMIN_ID).await;
    s.server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&new_task(Some(json!([]))))
        .await
        .assert_status_ok();

    let res = s
        .server
        .put("/api/v1/projects/test-project/git-host-connections")
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
    assert_eq!(s.db.rows::<git_host_connection::Entity>().await.len(), 1);
    assert!(s.db.rows::<task_trigger::Entity>().await.is_empty());
    assert!(s.db.rows::<task_action::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn suggestions_offer_push_and_pull_request_triggers_of_the_matching_webhook_receiver() {
    let s = seeded(BASE_ROLE_VIEW_ID).await;
    gradient_db::rows::insert::<git_host_connection::Entity, _>(
        &s.db.connection(),
        test_project_gitea_git_host_connection_for_webhook_events(),
    )
    .await
    .unwrap();

    let suggest = |repository: &'static str| {
        s.server
            .get("/api/v1/projects/test-project/trigger-suggestions")
            .add_query_param("repository", repository)
            .add_header("authorization", &s.auth)
    };

    let res = suggest("git@gitea.example.com:acme/app.git").await;
    res.assert_status_ok();
    let suggested: Vec<TriggerConfig> =
        serde_json::from_value(res.json::<Value>()["message"].clone()).unwrap();
    assert_eq!(
        suggested
            .iter()
            .map(TriggerConfig::trigger_type)
            .collect::<Vec<_>>(),
        vec![TriggerType::ReporterPush, TriggerType::ReporterPullRequest]
    );

    let other = suggest("https://codeberg.org/acme/app.git").await;
    other.assert_status_ok();
    assert_eq!(other.json::<Value>()["message"], json!([]));
    assert!(s.db.rows::<task_trigger::Entity>().await.is_empty());
    s.db.finish().await;
}

#[tokio::test]
async fn duplicating_a_task_copies_settings_triggers_and_actions_onto_the_new_branch() {
    let s = seeded(BASE_ROLE_ADMIN_ID).await;
    let conn = s.db.connection();
    let source = task::Model {
        exec_enabled: true,
        wait_for_workers: true,
        ..rows::task()
    };
    gradient_db::rows::insert::<task::Entity, _>(&conn, source.clone())
        .await
        .unwrap();
    for (config, is_default) in [
        (
            TriggerConfig::Polling {
                interval_secs: 300,
                branch: None,
            },
            true,
        ),
        (
            TriggerConfig::ReporterPush {
                branches: vec!["main".into()],
                tags: vec![],
                releases_only: false,
                include_flake_inputs: false,
            },
            false,
        ),
    ] {
        gradient_db::triggers::insert(
            &conn,
            task_trigger::Model {
                id: TaskTriggerId::now_v7(),
                task: task_id(),
                trigger_type: config.trigger_type(),
                config: config.to_db_json(),
                active: true,
                is_default,
                last_fired_at: Some(test_date()),
                created_at: test_date(),
                updated_at: test_date(),
            },
        )
        .await
        .unwrap();
    }
    gradient_db::actions::insert(
        &conn,
        task_action::Model {
            id: TaskActionId::now_v7(),
            task: task_id(),
            name: "notify".into(),
            action_type: gradient_types::actions::ActionType::SendWebRequest,
            config: json!({ "type": "send_web_request", "url": "https://hooks.example.com" }),
            events: json!(["evaluation.failed"]),
            active: true,
            created_by: user_id(),
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let res = s
        .server
        .post("/api/v1/tasks/test-project/test-task/duplicate")
        .add_header("authorization", &s.auth)
        .json(
            &json!({ "name": "test-task-release", "display_name": "Release", "branch": "release" }),
        )
        .await;

    res.assert_status_ok();
    let copy_id: TaskId = res.json::<Value>()["message"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let copy = s.db.find::<task::Entity>(copy_id).await.unwrap();
    assert_eq!(copy.name, "test-task-release");
    assert!(copy.exec_enabled && copy.wait_for_workers);
    assert_eq!(copy.repository, source.repository);

    let triggers: Vec<_> =
        s.db.rows::<task_trigger::Entity>()
            .await
            .into_iter()
            .filter(|t| t.task == copy_id)
            .collect();
    assert!(triggers.iter().all(|t| t.last_fired_at.is_none()));
    let mut copied = configs(&triggers);
    copied.sort_by_key(|(c, _)| c.trigger_type() as i16);
    assert!(
        copied
            .iter()
            .all(|(c, _)| c.trigger_branch() == Some("release"))
    );
    assert_eq!(copied.iter().filter(|(_, default)| *default).count(), 1);

    let actions: Vec<_> =
        s.db.rows::<task_action::Entity>()
            .await
            .into_iter()
            .filter(|a| a.task == copy_id)
            .collect();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].name, "notify");
    s.db.finish().await;
}

#[tokio::test]
async fn the_repository_check_refuses_invalid_urls_and_view_members() {
    let admin = seeded(BASE_ROLE_ADMIN_ID).await;
    admin
        .server
        .post("/api/v1/projects/test-project/repository-check")
        .add_header("authorization", &admin.auth)
        .json(&json!({ "repository": "not a url" }))
        .await
        .assert_status_bad_request();
    admin.db.finish().await;

    let viewer = seeded(BASE_ROLE_VIEW_ID).await;
    let res = viewer
        .server
        .post("/api/v1/projects/test-project/repository-check")
        .add_header("authorization", &viewer.auth)
        .json(&json!({ "repository": "https://gitea.example.com/acme/app.git" }))
        .await;
    assert!(res.status_code().is_client_error());
    assert_ne!(res.status_code(), 400);
    viewer.db.finish().await;
}

#[tokio::test]
async fn the_task_names_the_branches_of_its_active_triggers_and_its_next_poll() {
    let s = seeded(BASE_ROLE_ADMIN_ID).await;
    s.server
        .put("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .json(&new_task(Some(json!([
            { "type": "polling", "interval_secs": 120, "branch": "release" },
            { "type": "time", "cron": "0 3 * * *" },
        ]))))
        .await
        .assert_status_ok();

    let res = s
        .server
        .get("/api/v1/tasks/test-project/app")
        .add_header("authorization", &s.auth)
        .await;

    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["message"]["branches"], json!([null, "release"]));
    assert!(body["message"]["next_polling_trigger_check_at"].is_string());

    let list = s
        .server
        .get("/api/v1/tasks/test-project")
        .add_header("authorization", &s.auth)
        .await;
    list.assert_status_ok();
    assert_eq!(
        list.json::<Value>()["message"]["items"][0]["branches"],
        json!([null, "release"])
    );

    let details = s
        .server
        .get("/api/v1/tasks/test-project/app/details")
        .add_header("authorization", &s.auth)
        .await;
    details.assert_status_ok();
    let details = details.json::<Value>();
    assert_eq!(details["message"]["branches"], json!([null, "release"]));
    assert!(details["message"]["next_polling_trigger_check_at"].is_string());
    s.db.finish().await;
}
