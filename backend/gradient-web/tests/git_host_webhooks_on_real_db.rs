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
use gradient_entity::{commit, evaluation, git_host_connection, task, task_trigger};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::rows;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_types::ids::*;
use gradient_types::triggers::{ChangedFilePathFilter, TriggerConfig};
use gradient_types::{GitHostType, branch_key};
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
    repository: &str,
    path_filter: ChangedFilePathFilter,
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
            repository: repository.into(),
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
        path_filter,
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
    body: &[u8],
) -> TestResponse {
    server
        .post(path)
        .add_header("X-Gitea-Event", "push")
        .add_header(
            "X-Gitea-Signature",
            compute_gitea_webhook_signature_hex(plaintext_secret, body),
        )
        .bytes(body.to_vec().into())
        .await
}

fn commit_files(repo: &git2::Repository, message: &str, paths: &[&str]) -> git2::Oid {
    let author = git2::Signature::now("Test User", "test@example.invalid").unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    let parent_tree = match &parent {
        Some(commit) => commit.tree().unwrap(),
        None => repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap(),
    };
    let mut update = git2::build::TreeUpdateBuilder::new();
    for path in paths {
        update.upsert(
            *path,
            repo.blob(message.as_bytes()).unwrap(),
            git2::FileMode::Blob,
        );
    }
    let tree = repo
        .find_tree(update.create_updated(repo, &parent_tree).unwrap())
        .unwrap();
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    repo.commit(Some("HEAD"), &author, &author, message, &tree, &parents)
        .unwrap()
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
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
        )
        .await;

    let res = post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        secret,
        GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH.as_bytes(),
    )
    .await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn push_webhook_with_wrong_signature_counts_on_the_connection_without_a_delivery_row() {
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            "webhook-secret-plaintext",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
        )
        .await;

    for _ in 0..3 {
        let res = post_signed_gitea_push_webhook(
            &s.server,
            &format!("/api/v1/hooks/{}", s.connection.id),
            "a-different-secret",
            GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH.as_bytes(),
        )
        .await;
        res.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    let connection = s
        .db
        .find::<git_host_connection::Entity>(s.connection.id)
        .await
        .unwrap();
    assert_eq!(connection.refused_signature_count, 3);
    assert!(connection.last_refused_signature_at.is_some());
    let deliveries =
        gradient_db::incoming_deliveries::of_connection(&s.db.connection(), s.connection.id, 50)
            .await
            .unwrap();
    assert!(deliveries.is_empty(), "{deliveries:?}");
    s.db.finish().await;
}

#[tokio::test]
async fn test_signed_push_webhook_at_old_gitea_hook_path_queues_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
        )
        .await;

    let res = post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/gitea/test-project/{}", s.connection.name),
        secret,
        GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH.as_bytes(),
    )
    .await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &res).await;
    s.db.finish().await;
}

#[tokio::test]
async fn signed_gitea_push_webhook_with_no_matching_include_path_skips_evaluation_with_path_filter_reason()
 {
    let secret = "webhook-secret-plaintext";
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    let base = commit_files(&repo, "base", &["src/main.rs"]);
    let head = commit_files(&repo, "head", &["docs/guide.md"]);
    let url = format!("file://{}", repo_dir.path().display());
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            &url,
            ChangedFilePathFilter {
                include_paths: vec!["src/*".into()],
                exclude_paths: vec![],
            },
        )
        .await;
    let conn = s.db.connection();
    let base_commit = gradient_db::rows::insert::<commit::Entity, _>(
        &conn,
        commit::Model {
            hash: base.as_bytes().to_vec(),
            ..rows::build_request_commit()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<evaluation::Entity, _>(
        &conn,
        evaluation::Model {
            branch: branch_key::branch("main"),
            status: evaluation::EvaluationStatus::Completed,
            ..rows::queued_evaluation(task_id(), base_commit.id)
        },
    )
    .await
    .unwrap();
    let body = json!({
        "ref": "refs/heads/main",
        "after": head.to_string(),
        "repository": { "clone_url": url, "ssh_url": url },
    })
    .to_string();

    let res = post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        secret,
        body.as_bytes(),
    )
    .await;

    res.assert_status_ok();
    let message = res.json::<Value>()["message"].clone();
    let skipped = message["skipped"].as_array().unwrap();
    assert_eq!(
        (
            message["queued"].as_array().unwrap().len(),
            skipped.len(),
            skipped[0]["reason"].clone(),
        ),
        (0, 1, json!("no_changed_file_matches_path_filter")),
        "{message}"
    );
    assert_eq!(s.db.rows::<evaluation::Entity>().await.len(), 1);
    s.db.finish().await;
}

#[tokio::test]
async fn gitea_fork_pull_request_from_untrusted_sender_waits_for_approval_then_queues_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
        )
        .await;
    let conn = s.db.connection();
    let pull_request = TriggerConfig::ReporterPullRequest {
        branches: vec![],
        actions: vec![],
        require_approval: true,
        path_filter: ChangedFilePathFilter::default(),
    };
    gradient_db::triggers::insert(
        &conn,
        task_trigger::Model {
            id: TaskTriggerId::now_v7(),
            task: task_id(),
            trigger_type: pull_request.trigger_type(),
            config: pull_request.to_db_json(),
            active: true,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let head_sha = "abcdef0123456789abcdef0123456789abcdef01";
    let body = json!({
        "action": "opened",
        "number": 7,
        "pull_request": {
            "number": 7,
            "user": { "login": "external" },
            "head": {
                "sha": head_sha,
                "ref": "feature",
                "repo": {
                    "full_name": "external/repo",
                    "clone_url": "https://gitea.example.com/external/repo",
                },
            },
        },
        "repository": {
            "full_name": "test-org/repo",
            "clone_url": "https://gitea.example.com/test-org/repo",
        },
        "sender": { "login": "external" },
    })
    .to_string();
    let build_signed_pull_request_webhook_request = || {
        s.server
            .post(&format!("/api/v1/hooks/{}", s.connection.id))
            .add_header("X-Gitea-Event", "pull_request")
            .add_header(
                "X-Gitea-Signature",
                compute_gitea_webhook_signature_hex(secret, body.as_bytes()),
            )
            .bytes(body.clone().into_bytes().into())
    };

    let waiting = build_signed_pull_request_webhook_request().await;

    waiting.assert_status_ok();
    let message = waiting.json::<Value>()["message"].clone();
    assert_eq!(
        message["skipped"][0]["reason"],
        json!("fork_pull_request_commit_waits_for_maintainer_approval"),
        "{message}"
    );
    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    let approvals =
        s.db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
            .await;
    assert_eq!(approvals.len(), 1);
    assert_eq!(
        (
            approvals[0].pr_number,
            approvals[0].commit_hash.clone(),
            approvals[0].head_repo_clone_url.as_deref(),
        ),
        (
            7,
            hex::decode(head_sha).unwrap(),
            Some("https://gitea.example.com/external/repo"),
        )
    );

    gradient_db::fork_pull_request_approvals::mark_pull_request_approval_approved(
        &conn,
        approvals[0].id,
        "maintainer",
    )
    .await
    .unwrap();
    let approved = build_signed_pull_request_webhook_request().await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &approved).await;
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

#[tokio::test]
async fn a_new_github_app_installation_links_tasks_of_the_installed_account() {
    let secret = "github-webhook-secret";
    let secret_file = write_content_to_new_temp_file(secret);
    let db = TestDatabase::create().await;
    let conn = db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::superuser())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(&conn, rows::project())
        .await
        .unwrap();
    for (name, repository) in [
        ("own", "https://github.com/gh-org/app"),
        ("foreign", "https://github.com/other/app"),
    ] {
        gradient_db::rows::insert::<task::Entity, _>(
            &conn,
            task::Model {
                id: TaskId::now_v7(),
                name: name.into(),
                repository: repository.into(),
                ..rows::task()
            },
        )
        .await
        .unwrap();
    }
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| {
            cli.github_app.id = std::num::NonZeroU64::new(1234);
            cli.github_app.private_key_file = Some("/dev/null".into());
            cli.github_app.webhook_secret_file = Some(secret_file);
        })
        .server();
    let body = serde_json::to_vec(&json!({
        "action": "created",
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
        .await
        .assert_status_ok();

    let connection = db.rows::<git_host_connection::Entity>().await.remove(0);
    let tasks = db.rows::<task::Entity>().await;
    let own = tasks.iter().find(|t| t.name == "own").unwrap();
    let foreign = tasks.iter().find(|t| t.name == "foreign").unwrap();
    assert_eq!(own.git_host_connection, Some(connection.id));
    assert!(own.report_status);
    assert_eq!(foreign.git_host_connection, None);
    db.finish().await;
}
