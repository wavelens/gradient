/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::{TestResponse, TestServer};
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_entity::{evaluation, git_host_connection, task, task_trigger};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_types::GitHostType;
use gradient_types::ids::*;
use gradient_types::triggers::TriggerConfig;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

const GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

fn compute_gitea_webhook_signature_hex(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn write_content_to_new_temp_file(content: &str) -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-secret-{}", Uuid::now_v7()));
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().into_owned()
}

struct WebhookTestRealDatabaseHttpServerAndGiteaConnection {
    db: TestDatabase,
    server: TestServer,
    connection: git_host_connection::Model,
}

async fn create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
    plaintext_secret: &str,
) -> WebhookTestRealDatabaseHttpServerAndGiteaConnection {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    let crypt = write_content_to_new_temp_file("this-is-a-32-byte-crypt-key!!!!");
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::user())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(&conn, rows::project())
        .await
        .unwrap();
    let connection = gradient_db::rows::insert::<git_host_connection::Entity, _>(
        &conn,
        git_host_connection::Model {
            id: GitHostConnectionId::now_v7(),
            project: Some(project_id()),
            name: "gitea".into(),
            display_name: "Gitea".into(),
            git_host_type: GitHostType::Gitea,
            base_url: Some("https://gitea.example.com".into()),
            secret: Some(encrypt_secret_with_file(&crypt, plaintext_secret).unwrap()),
            receives_events: true,
            created_by: user_id(),
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<task::Entity, _>(
        &conn,
        task::Model {
            repository: "https://gitea.example.com/test-org/repo".into(),
            git_host_connection: Some(connection.id),
            ..rows::task()
        },
    )
    .await
    .unwrap();
    let push = TriggerConfig::ReporterPush {
        branches: vec![],
        tags: vec![],
        releases_only: false,
        include_flake_inputs: false,
    };
    gradient_db::triggers::insert(
        &conn,
        task_trigger::Model {
            id: TaskTriggerId::now_v7(),
            task: task_id(),
            trigger_type: push.trigger_type(),
            config: push.to_db_json(),
            active: true,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| cli.secrets.crypt_file = crypt)
        .server();
    WebhookTestRealDatabaseHttpServerAndGiteaConnection {
        db,
        server,
        connection,
    }
}

async fn post_signed_gitea_push_webhook(
    server: &TestServer,
    path: &str,
    plaintext_secret: &str,
) -> TestResponse {
    let body = GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH.as_bytes();
    server
        .post(path)
        .add_header("X-Gitea-Event", "push")
        .add_header(
            "X-Gitea-Signature",
            compute_gitea_webhook_signature_hex(plaintext_secret, body),
        )
        .bytes(body.into())
        .await
}

async fn assert_webhook_queued_one_evaluation_and_evaluation_row_exists(
    db: &TestDatabase,
    res: &TestResponse,
) {
    res.assert_status_ok();
    let message = res.json::<Value>()["message"].clone();
    assert_eq!(message["queued"].as_array().unwrap().len(), 1, "{message}");
    let evaluations = db.rows::<evaluation::Entity>().await;
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0].task, Some(task_id()));
}

#[tokio::test]
async fn signed_push_webhook_queues_evaluation_for_task_using_git_host_connection() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
        )
        .await;

    let res = post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        secret,
    )
    .await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn test_signed_push_webhook_at_old_gitea_hook_path_queues_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
        )
        .await;

    let res = post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/gitea/test-project/{}", s.connection.name),
        secret,
    )
    .await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn github_app_installation_webhook_created_stores_and_deleted_removes_instance_git_host_connection()
 {
    let secret = "github-webhook-secret";
    let secret_file = write_content_to_new_temp_file(secret);
    let db = TestDatabase::create().await;
    let conn = db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::superuser())
        .await
        .unwrap();
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| {
            cli.github_app.id = std::num::NonZeroU64::new(1234);
            cli.github_app.private_key_file = Some("/dev/null".into());
            cli.github_app.webhook_secret_file = Some(secret_file);
        })
        .server();
    let build_signed_installation_event_request = |action: &str| {
        let body = serde_json::to_vec(&json!({
            "action": action,
            "installation": { "id": 9999, "account": { "login": "gh-org" } },
            "sender": { "login": "some-user" },
        }))
        .unwrap();
        let signature = format!(
            "sha256={}",
            compute_gitea_webhook_signature_hex(secret, &body)
        );
        server
            .post("/api/v1/hooks/github")
            .add_header("X-GitHub-Event", "installation")
            .add_header("X-Hub-Signature-256", signature)
            .bytes(body.into())
    };

    build_signed_installation_event_request("created")
        .await
        .assert_status_ok();
    let stored = db.rows::<git_host_connection::Entity>().await;
    build_signed_installation_event_request("deleted")
        .await
        .assert_status_ok();

    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].github_installation_id, Some(9999));
    assert_eq!(stored[0].git_host_type, GitHostType::GitHub);
    assert_eq!(stored[0].project, None);
    assert_eq!(stored[0].owner_team, None);
    assert_eq!(stored[0].account_login.as_deref(), Some("gh-org"));
    assert!(db.rows::<git_host_connection::Entity>().await.is_empty());
    db.finish().await;
}
