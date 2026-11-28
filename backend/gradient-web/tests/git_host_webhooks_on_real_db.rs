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
use gradient_test_support::git_daemon_and_fork_repository_fixtures::{
    create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash,
    start_git_daemon_on_free_loopback_port_serving_base_directory,
};
use gradient_test_support::state::TestState;
use gradient_types::ids::*;
use gradient_types::triggers::{ChangedFilePathFilter, TriggerConfig};
use gradient_types::{GitHostType, branch_key};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate, Times};

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
    git_server_base_url: &str,
    repository: &str,
    path_filter: ChangedFilePathFilter,
    project_owning_git_host_connection: Option<ProjectId>,
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
            project: project_owning_git_host_connection,
            name: "gitea".into(),
            display_name: "Gitea".into(),
            git_host_type: GitHostType::Gitea,
            base_url: Some(git_server_base_url.into()),
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
    post_signed_gitea_webhook_with_event_name(server, path, "push", plaintext_secret, body).await
}

async fn post_signed_gitea_webhook_with_event_name(
    server: &TestServer,
    path: &str,
    event_name: &str,
    plaintext_secret: &str,
    body: &[u8],
) -> TestResponse {
    server
        .post(path)
        .add_header("X-Gitea-Event", event_name)
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
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
            Some(project_id()),
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
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
            Some(project_id()),
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

    let connection =
        s.db.find::<git_host_connection::Entity>(s.connection.id)
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
async fn push_for_untracked_repository_records_which_repository_no_task_tracks() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/another-repo",
            ChangedFilePathFilter::default(),
            Some(project_id()),
        )
        .await;

    post_signed_gitea_push_webhook(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        secret,
        GITEA_PUSH_WEBHOOK_JSON_BODY_FOR_MAIN_BRANCH.as_bytes(),
    )
    .await
    .assert_status_ok();

    let deliveries =
        gradient_db::incoming_deliveries::of_connection(&s.db.connection(), s.connection.id, 50)
            .await
            .unwrap();
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert_eq!(
        deliveries[0].detail.as_deref(),
        Some("No task tracks test-org/repo.")
    );
    s.db.finish().await;
}

#[tokio::test]
async fn test_signed_push_webhook_at_old_gitea_hook_path_queues_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
            Some(project_id()),
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
    let served = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(served.path().join("test-org/repo")).unwrap();
    let base = commit_files(&repo, "base", &["src/main.rs"]);
    let head = commit_files(&repo, "head", &["docs/guide.md"]);
    let (_daemon, daemon_url) =
        start_git_daemon_on_free_loopback_port_serving_base_directory(served.path());
    let url = format!("{daemon_url}/test-org/repo");
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "http://127.0.0.1",
            &url,
            ChangedFilePathFilter {
                include_paths: vec!["src/*".into()],
                exclude_paths: vec![],
            },
            Some(project_id()),
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
async fn gitea_fork_pull_request_webhook_with_approval_required_and_unmatched_path_filter_creates_pending_approval_row_without_fetching_fork_content()
 {
    let secret = "webhook-secret-plaintext";
    let served = tempfile::tempdir().unwrap();
    let head =
        create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash(
            served.path(),
            "src/main.rs",
            "docs/guide.md",
        );
    let (_daemon, daemon_url) =
        start_git_daemon_on_free_loopback_port_serving_base_directory(served.path());
    let url = format!("{daemon_url}/test-org/repo");
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "http://127.0.0.1",
            &url,
            ChangedFilePathFilter::default(),
            Some(project_id()),
        )
        .await;
    let pull_request = TriggerConfig::ReporterPullRequest {
        branches: vec![],
        actions: vec![],
        require_approval: true,
        path_filter: ChangedFilePathFilter {
            include_paths: vec!["src/*".into()],
            exclude_paths: vec![],
        },
    };
    gradient_db::triggers::insert(
        &s.db.connection(),
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
    let body = json!({
        "action": "opened",
        "number": 7,
        "pull_request": {
            "number": 7,
            "user": { "login": "contributor" },
            "head": {
                "sha": head,
                "ref": "feature",
                "repo": {
                    "full_name": "external/repo",
                    "clone_url": format!("{daemon_url}/external/repo"),
                },
            },
            "base": { "sha": "0".repeat(40), "ref": "main" },
        },
        "repository": { "full_name": "test-org/repo", "clone_url": url },
        "sender": { "login": "contributor" },
    })
    .to_string();

    let res = post_signed_gitea_webhook_with_event_name(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        "pull_request",
        secret,
        body.as_bytes(),
    )
    .await;

    res.assert_status_ok();
    let message = res.json::<Value>()["message"].clone();
    assert_eq!(
        message["skipped"][0]["reason"],
        json!("fork_pull_request_commit_waits_for_maintainer_approval"),
        "{message}"
    );
    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    let approvals =
        s.db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
            .await;
    assert_eq!(
        approvals
            .iter()
            .map(|approval| approval.pr_base_branch_name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("main")]
    );
    s.db.finish().await;
}

#[tokio::test]
async fn gitea_fork_pull_request_from_untrusted_sender_waits_for_approval_then_queues_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
            Some(project_id()),
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
    let pushed_after_approval_sha = "0123456789abcdef0123456789abcdef01234567";
    let hook_path = format!("/api/v1/hooks/{}", s.connection.id);
    let post_signed_pull_request_webhook_for_head = |head: &str| {
        let body = build_gitea_pull_request_webhook_json_body(
            "opened",
            "external",
            head,
            "https://gitea.example.com/test-org/repo",
        );
        let path = hook_path.clone();
        let server = &s.server;
        async move {
            post_signed_gitea_webhook_with_event_name(
                server,
                &path,
                "pull_request",
                secret,
                body.as_bytes(),
            )
            .await
        }
    };

    let waiting = post_signed_pull_request_webhook_for_head(head_sha).await;

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
    let approved = post_signed_pull_request_webhook_for_head(head_sha).await;

    assert_webhook_queued_one_evaluation_and_evaluation_row_exists(&s.db, &approved).await;
    assert!(
        s.db.rows::<evaluation::Entity>().await[0]
            .allows_project_credentials_and_privileged_actions
    );

    let pushed_after_approval =
        post_signed_pull_request_webhook_for_head(pushed_after_approval_sha).await;

    pushed_after_approval.assert_status_ok();
    let message = pushed_after_approval.json::<Value>()["message"].clone();
    assert_eq!(
        message["skipped"][0]["reason"],
        json!("fork_pull_request_commit_waits_for_maintainer_approval"),
        "{message}"
    );
    assert_eq!(s.db.rows::<evaluation::Entity>().await.len(), 1);
    let pending_commits: Vec<Vec<u8>> =
        s.db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
            .await
            .into_iter()
            .filter(|approval| approval.approved_at.is_none())
            .map(|approval| approval.commit_hash)
            .collect();
    assert_eq!(
        pending_commits,
        vec![hex::decode(pushed_after_approval_sha).unwrap()]
    );
    s.db.finish().await;
}

fn build_gitea_pull_request_webhook_json_body(
    action: &str,
    sender: &str,
    head_sha: &str,
    repository_clone_url: &str,
) -> String {
    json!({
        "action": action,
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
            "clone_url": repository_clone_url,
        },
        "sender": { "login": sender },
    })
    .to_string()
}

fn build_gitea_issue_comment_webhook_body(
    comment: &str,
    comment_created_at: chrono::NaiveDateTime,
    repository_clone_url: &str,
) -> String {
    json!({
        "action": "created",
        "comment": {
            "id": 11,
            "body": comment,
            "created_at": comment_created_at
                .and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        "issue": { "number": 7 },
        "sender": { "login": "maintainer" },
        "repository": {
            "full_name": "test-org/repo",
            "clone_url": repository_clone_url,
        },
    })
    .to_string()
}

struct ForkPullRequestApprovalTestFixture {
    db: TestDatabase,
    server: TestServer,
    connection: git_host_connection::Model,
    git_host: MockServer,
    repository_clone_url: String,
    require_approval_pull_request_trigger_id: TaskTriggerId,
}

async fn set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
    plaintext_secret: &str,
    expected_maintainer_push_permission_check_count: impl Into<Times>,
) -> ForkPullRequestApprovalTestFixture {
    let git_host = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/repos/test-org/repo/collaborators/maintainer/permission",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "permission": "write" })))
        .expect(expected_maintainer_push_permission_check_count)
        .mount(&git_host)
        .await;
    create_test_database_with_project_task_and_approval_required_pull_request_trigger_for_mock_git_host(
        git_host,
        GitHostType::Gitea,
        plaintext_secret,
    )
    .await
}

async fn create_test_database_with_project_task_and_approval_required_pull_request_trigger_for_mock_git_host(
    git_host: MockServer,
    git_host_type: GitHostType,
    plaintext_secret: &str,
) -> ForkPullRequestApprovalTestFixture {
    let repository_clone_url = format!("{}/test-org/repo", git_host.uri());
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
            git_host_type,
            base_url: Some(git_host.uri()),
            secret: Some(encrypt_secret_with_file(&crypt, plaintext_secret).unwrap()),
            access_token: Some(encrypt_secret_with_file(&crypt, "token").unwrap()),
            receives_events: true,
            reports_status: true,
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
            repository: repository_clone_url.clone(),
            git_host_connection: Some(connection.id),
            report_status: true,
            ..rows::task()
        },
    )
    .await
    .unwrap();
    let pull_request = TriggerConfig::ReporterPullRequest {
        branches: vec![],
        actions: vec![],
        require_approval: true,
        path_filter: ChangedFilePathFilter::default(),
    };
    let trigger = gradient_db::triggers::insert(
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

    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| cli.secrets.crypt_file = crypt)
        .server();
    ForkPullRequestApprovalTestFixture {
        db,
        server,
        connection,
        git_host,
        repository_clone_url,
        require_approval_pull_request_trigger_id: trigger.id,
    }
}

async fn insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
    s: &ForkPullRequestApprovalTestFixture,
    commit_sha: &str,
    recorded_at: chrono::NaiveDateTime,
) {
    gradient_db::fork_pull_request_approvals::add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
        &s.db.connection(),
        gradient_entity::fork_pull_request_approval::Model {
            id: PullRequestApprovalId::now_v7(),
            task: task_id(),
            trigger: s.require_approval_pull_request_trigger_id,
            pr_number: 7,
            pr_author: "external".into(),
            commit_hash: hex::decode(commit_sha).unwrap(),
            head_repo_clone_url: Some("https://gitea.example.com/external/repo".into()),
            created_at: recorded_at,
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

async fn register_mock_gitea_pull_request_7_with_head_commit(
    s: &ForkPullRequestApprovalTestFixture,
    head_sha: &str,
) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/test-org/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 7,
            "title": "Fork change",
            "user": { "login": "external" },
            "head": {
                "sha": head_sha,
                "ref": "feature",
                "label": "external:feature",
                "repo": {
                    "full_name": "external/repo",
                    "clone_url": "https://gitea.example.com/external/repo",
                },
            },
            "base": {
                "sha": "cccccccccccccccccccccccccccccccccccccccc",
                "ref": "main",
                "label": "main",
                "repo": {
                    "full_name": "test-org/repo",
                    "clone_url": s.repository_clone_url,
                },
            },
        })))
        .mount(&s.git_host)
        .await;
}

async fn register_mock_gitea_pull_request_reply_comment_with_named_commit(
    s: &ForkPullRequestApprovalTestFixture,
    commit_sha: &str,
    expected_reply_count: u64,
) {
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/test-org/repo/issues/7/comments"))
        .and(body_string_contains(commit_sha))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 12 })))
        .expect(expected_reply_count)
        .mount(&s.git_host)
        .await;
}

async fn send_signed_gitea_issue_comment_webhook_with_comment_created_at(
    s: &ForkPullRequestApprovalTestFixture,
    plaintext_secret: &str,
    comment: &str,
    comment_created_at: chrono::NaiveDateTime,
) {
    post_signed_gitea_webhook_with_event_name(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        "issue_comment",
        plaintext_secret,
        build_gitea_issue_comment_webhook_body(
            comment,
            comment_created_at,
            &s.repository_clone_url,
        )
        .as_bytes(),
    )
    .await
    .assert_status_ok();
}

async fn read_stored_pull_request_approval_rows(
    db: &TestDatabase,
) -> Vec<(Vec<u8>, Option<String>, bool)> {
    db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
        .await
        .into_iter()
        .map(|approval| {
            (
                approval.commit_hash,
                approval.approving_maintainer_login,
                approval.approved_at.is_some(),
            )
        })
        .collect()
}

async fn send_signed_gitea_fork_pull_request_webhook_as_maintainer_and_assert_pending_approval_without_push_permission_lookup(
    action: &str,
) {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 0,
        )
        .await;
    let head_sha = "abcdef0123456789abcdef0123456789abcdef01";
    let body = build_gitea_pull_request_webhook_json_body(
        action,
        "maintainer",
        head_sha,
        &s.repository_clone_url,
    );

    let res = post_signed_gitea_webhook_with_event_name(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        "pull_request",
        secret,
        body.as_bytes(),
    )
    .await;

    res.assert_status_ok();
    let message = res.json::<Value>()["message"].clone();
    assert_eq!(
        (
            message["queued"].as_array().unwrap().len(),
            message["skipped"][0]["reason"].clone(),
        ),
        (
            0,
            json!("fork_pull_request_commit_waits_for_maintainer_approval")
        ),
        "{message}"
    );
    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    let approvals =
        s.db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
            .await;
    assert_eq!(
        approvals
            .iter()
            .map(|approval| (approval.commit_hash.clone(), approval.approved_at))
            .collect::<Vec<_>>(),
        vec![(hex::decode(head_sha).unwrap(), None)]
    );
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn test_maintainer_synchronize_fork_pull_request_starts_no_evaluation_and_stores_pending_approval()
 {
    send_signed_gitea_fork_pull_request_webhook_as_maintainer_and_assert_pending_approval_without_push_permission_lookup(
        "synchronize",
    )
    .await;
}

#[tokio::test]
async fn gitea_reopened_fork_pull_request_by_maintainer_with_push_does_not_start_evaluation_and_stores_one_pending_approval()
 {
    send_signed_gitea_fork_pull_request_webhook_as_maintainer_and_assert_pending_approval_without_push_permission_lookup(
        "reopened",
    )
    .await;
}

#[tokio::test]
async fn fork_push_arriving_before_approval_comment_webhook_approves_nothing_starts_no_evaluation_keeps_new_commit_pending_and_replies_with_full_head_commit_hash()
 {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 1..,
        )
        .await;
    let reviewed_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let pushed_after_comment_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let comment_created_at = gradient_types::now() - chrono::TimeDelta::minutes(10);
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        reviewed_sha,
        comment_created_at - chrono::TimeDelta::minutes(20),
    )
    .await;
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        pushed_after_comment_sha,
        comment_created_at + chrono::TimeDelta::seconds(1),
    )
    .await;
    register_mock_gitea_pull_request_7_with_head_commit(&s, pushed_after_comment_sha).await;
    register_mock_gitea_pull_request_reply_comment_with_named_commit(
        &s,
        pushed_after_comment_sha,
        3,
    )
    .await;

    for comment in [
        "/gradient approve".to_owned(),
        format!("/gradient approve {reviewed_sha}"),
        "/gradient run".to_owned(),
    ] {
        send_signed_gitea_issue_comment_webhook_with_comment_created_at(
            &s,
            secret,
            &comment,
            comment_created_at,
        )
        .await;
    }

    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(hex::decode(pushed_after_comment_sha).unwrap(), None, false)]
    );
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn bare_approve_ten_minutes_after_recorded_head_commit_approves_and_starts_evaluation() {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 1..,
        )
        .await;
    let head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let comment_created_at = gradient_types::now() - chrono::TimeDelta::minutes(10);
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        head_sha,
        comment_created_at - chrono::TimeDelta::minutes(10),
    )
    .await;
    register_mock_gitea_pull_request_7_with_head_commit(&s, head_sha).await;

    send_signed_gitea_issue_comment_webhook_with_comment_created_at(
        &s,
        secret,
        "/gradient approve",
        comment_created_at,
    )
    .await;

    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(
            hex::decode(head_sha).unwrap(),
            Some("maintainer".to_owned()),
            true
        )]
    );
    assert_eq!(s.db.rows::<evaluation::Entity>().await.len(), 1);
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn bare_approve_five_seconds_after_recorded_head_is_refused_despite_2020_commit_dates_and_reply_names_head()
 {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 1..,
        )
        .await;
    let head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut delivery: Value = serde_json::from_str(&build_gitea_pull_request_webhook_json_body(
        "synchronize",
        "external",
        head_sha,
        &s.repository_clone_url,
    ))
    .unwrap();
    delivery["pull_request"]["head"]["commit"] = json!({
        "author": { "date": "2020-01-01T00:00:00Z" },
        "committer": { "date": "2020-01-01T00:00:00Z" },
    });
    post_signed_gitea_webhook_with_event_name(
        &s.server,
        &format!("/api/v1/hooks/{}", s.connection.id),
        "pull_request",
        secret,
        delivery.to_string().as_bytes(),
    )
    .await
    .assert_status_ok();
    let recorded_at =
        s.db.rows::<gradient_entity::fork_pull_request_approval::Entity>()
            .await[0]
            .created_at;
    register_mock_gitea_pull_request_7_with_head_commit(&s, head_sha).await;
    register_mock_gitea_pull_request_reply_comment_with_named_commit(&s, head_sha, 1).await;

    send_signed_gitea_issue_comment_webhook_with_comment_created_at(
        &s,
        secret,
        "/gradient approve",
        recorded_at + chrono::TimeDelta::seconds(5),
    )
    .await;

    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(hex::decode(head_sha).unwrap(), None, false)]
    );
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn approve_comment_from_read_permission_user_approves_nothing_and_posts_no_reply() {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 0,
        )
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/repos/test-org/repo/collaborators/maintainer/permission",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "permission": "read" })))
        .with_priority(1)
        .expect(1..)
        .mount(&s.git_host)
        .await;
    let head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let comment_created_at = gradient_types::now() - chrono::TimeDelta::minutes(10);
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        head_sha,
        comment_created_at - chrono::TimeDelta::minutes(10),
    )
    .await;
    register_mock_gitea_pull_request_7_with_head_commit(&s, head_sha).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/test-org/repo/issues/7/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 12 })))
        .expect(0)
        .mount(&s.git_host)
        .await;

    send_signed_gitea_issue_comment_webhook_with_comment_created_at(
        &s,
        secret,
        "/gradient approve",
        comment_created_at,
    )
    .await;

    assert!(s.db.rows::<evaluation::Entity>().await.is_empty());
    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(hex::decode(head_sha).unwrap(), None, false)]
    );
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn maintainer_posts_abort_comment_on_pull_request_7_aborts_its_running_evaluations_but_not_pull_request_8_or_completed_evaluation()
 {
    use evaluation::EvaluationStatus::{Aborted, Building, Completed, Queued};

    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 1,
        )
        .await;
    let conn = s.db.connection();
    let commit =
        gradient_db::rows::insert::<commit::Entity, _>(&conn, rows::build_request_commit())
            .await
            .unwrap();
    let mut evaluation_ids = Vec::new();
    for (branch, status, concurrent) in [
        (branch_key::pull_request(7), Building, false),
        (branch_key::pull_request(7), Queued, true),
        (branch_key::pull_request(7), Completed, false),
        (branch_key::pull_request(8), Building, false),
    ] {
        let inserted = gradient_db::rows::insert::<evaluation::Entity, _>(
            &conn,
            evaluation::Model {
                branch,
                status,
                concurrent,
                ..rows::queued_evaluation(task_id(), commit.id)
            },
        )
        .await
        .unwrap();
        evaluation_ids.push(inserted.id);
    }

    send_signed_gitea_issue_comment_webhook_with_comment_created_at(
        &s,
        secret,
        "/gradient abort",
        gradient_types::now(),
    )
    .await;

    let mut statuses = std::collections::HashMap::new();
    for row in s.db.rows::<evaluation::Entity>().await {
        statuses.insert(row.id, row.status);
    }
    assert_eq!(
        evaluation_ids
            .iter()
            .map(|id| statuses[id])
            .collect::<Vec<_>>(),
        vec![Aborted, Aborted, Completed, Building]
    );
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn approve_comment_with_full_head_hash_approves_five_seconds_after_recorded_commit() {
    let secret = "webhook-secret-plaintext";
    let s =
        set_up_test_database_and_mock_gitea_repository_for_pull_request_with_required_approval_and_write_permission(
            secret, 1..,
        )
        .await;
    let head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let comment_created_at = gradient_types::now() - chrono::TimeDelta::minutes(10);
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        head_sha,
        comment_created_at - chrono::TimeDelta::seconds(5),
    )
    .await;
    register_mock_gitea_pull_request_7_with_head_commit(&s, head_sha).await;

    send_signed_gitea_issue_comment_webhook_with_comment_created_at(
        &s,
        secret,
        &format!("/gradient approve {head_sha}"),
        comment_created_at,
    )
    .await;

    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(
            hex::decode(head_sha).unwrap(),
            Some("maintainer".to_owned()),
            true
        )]
    );
    assert_eq!(s.db.rows::<evaluation::Entity>().await.len(), 1);
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn gitlab_bare_approve_comment_ten_minutes_after_merge_request_head_commit_approves() {
    let secret = "webhook-secret-plaintext";
    let git_host = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/test-org%2Frepo/members/all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "username": "maintainer", "access_level": 30 },
        ])))
        .expect(1..)
        .mount(&git_host)
        .await;
    let s = create_test_database_with_project_task_and_approval_required_pull_request_trigger_for_mock_git_host(
        git_host,
        GitHostType::GitLab,
        secret,
    )
    .await;
    let head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let note_created_at = gradient_types::now() - chrono::TimeDelta::minutes(10);
    insert_pending_approval_for_pull_request_7_with_commit_and_gradient_recorded_time(
        &s,
        head_sha,
        note_created_at - chrono::TimeDelta::minutes(10),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/test-org%2Frepo/merge_requests/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": head_sha,
            "source_branch": "feature",
            "source_project_id": 2,
            "target_project_id": 1,
            "title": "Fork change",
        })))
        .mount(&s.git_host)
        .await;
    let note = json!({
        "object_kind": "note",
        "user": { "username": "maintainer" },
        "project": {
            "path_with_namespace": "test-org/repo",
            "web_url": s.repository_clone_url,
        },
        "object_attributes": {
            "id": 11,
            "note": "/gradient approve",
            "noteable_type": "MergeRequest",
            "action": "create",
            "created_at": note_created_at
                .and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        "merge_request": { "iid": 7 },
    })
    .to_string();

    s.server
        .post(&format!("/api/v1/hooks/{}", s.connection.id))
        .add_header("X-Gitlab-Event", "Note Hook")
        .add_header("X-Gitlab-Token", secret)
        .bytes(note.into_bytes().into())
        .await
        .assert_status_ok();

    assert_eq!(
        read_stored_pull_request_approval_rows(&s.db).await,
        vec![(
            hex::decode(head_sha).unwrap(),
            Some("maintainer".to_owned()),
            true
        )]
    );
    assert_eq!(s.db.rows::<evaluation::Entity>().await.len(), 1);
    s.git_host.verify().await;
    s.db.finish().await;
}

#[tokio::test]
async fn test_push_webhook_on_instance_connection_starts_evaluation_only_for_task_with_repository_grant()
 {
    let secret = "webhook-secret-plaintext";
    let s =
        create_test_database_with_user_project_task_gitea_webhook_secret_push_trigger_and_http_server(
            secret,
            "https://gitea.example.com",
            "https://gitea.example.com/test-org/repo",
            ChangedFilePathFilter::default(),
            None,
        )
        .await;
    let conn = s.db.connection();
    let ungranted_project = ProjectId::now_v7();
    let ungranted_task = TaskId::now_v7();
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(
        &conn,
        gradient_entity::project::Model {
            id: ungranted_project,
            name: "ungranted-project".into(),
            ..rows::project()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<task::Entity, _>(
        &conn,
        task::Model {
            id: ungranted_task,
            project: ungranted_project,
            repository: "https://gitea.example.com/test-org/repo".into(),
            git_host_connection: Some(s.connection.id),
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
        path_filter: ChangedFilePathFilter::default(),
    };
    gradient_db::triggers::insert(
        &conn,
        task_trigger::Model {
            id: TaskTriggerId::now_v7(),
            task: ungranted_task,
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
    gradient_db::git_host_connection_repository_grants::insert(
        &conn,
        gradient_db::git_host_connection_repository_grants::NewGitHostConnectionRepositoryGrant {
            connection: s.connection.id,
            project: project_id(),
            repository_owner: "test-org",
            repository_name: Some("repo"),
            source: gradient_types::RepositoryGrantSource::Admin,
            created_by: Some(user_id()),
        },
    )
    .await
    .unwrap();

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
async fn new_github_app_installation_creates_instance_connection_and_links_no_task_because_no_project_has_repository_grant()
 {
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
    assert_eq!(connection.github_installation_id, Some(9999));
    assert_eq!(own.git_host_connection, None);
    assert!(!own.report_status);
    assert_eq!(foreign.git_host_connection, None);
    db.finish().await;
}

#[tokio::test]
async fn a_repeated_installation_event_leaves_task_connection_choices_alone() {
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
    for name in ["unlinked", "silent"] {
        gradient_db::rows::insert::<task::Entity, _>(
            &conn,
            task::Model {
                id: TaskId::now_v7(),
                name: name.into(),
                repository: format!("https://github.com/gh-org/{name}"),
                ..rows::task()
            },
        )
        .await
        .unwrap();
    }
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cli(|cli| {
            cli.github_app.id = std::num::NonZeroU64::new(1234);
            cli.github_app.private_key_file = Some("/dev/null".into());
            cli.github_app.webhook_secret_file = Some(secret_file);
        })
        .server();
    let send = |event: &'static str, action: &str| {
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
            .add_header("X-GitHub-Event", event)
            .add_header("X-Hub-Signature-256", signature)
            .bytes(body.into())
    };

    send("installation", "created").await.assert_status_ok();
    let first_installation_connection_row =
        db.rows::<git_host_connection::Entity>().await.remove(0);
    db.execute(&format!(
        "UPDATE task SET git_host_connection = NULL, report_status = false WHERE name = 'unlinked'; \
         UPDATE task SET git_host_connection = '{}', report_status = false WHERE name = 'silent'",
        first_installation_connection_row.id
    ))
    .await;
    send("installation", "suspend").await.assert_status_ok();
    send("installation", "new_permissions_accepted")
        .await
        .assert_status_ok();
    send("installation_repositories", "removed")
        .await
        .assert_status_ok();

    let connection = db.rows::<git_host_connection::Entity>().await.remove(0);
    let tasks = db.rows::<task::Entity>().await;
    let unlinked = tasks.iter().find(|t| t.name == "unlinked").unwrap();
    let silent = tasks.iter().find(|t| t.name == "silent").unwrap();
    assert_eq!(unlinked.git_host_connection, None);
    assert!(!unlinked.report_status);
    assert_eq!(silent.git_host_connection, Some(connection.id));
    assert!(!silent.report_status);
    db.finish().await;
}

async fn database_and_github_app_server_with_task(
    task_managed: bool,
    secret: &str,
) -> (TestDatabase, TestServer) {
    let secret_file = write_content_to_new_temp_file(secret);
    let db = TestDatabase::create().await;
    let conn = db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::superuser())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(&conn, rows::project())
        .await
        .unwrap();
    gradient_db::rows::insert::<task::Entity, _>(
        &conn,
        task::Model {
            repository: "https://github.com/gh-org/app".into(),
            managed: task_managed,
            ..rows::task()
        },
    )
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
    (db, server)
}

async fn send_installation_created_event(server: &TestServer, secret: &str) {
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
}

#[tokio::test]
async fn a_new_github_app_installation_leaves_a_task_of_declarative_state_alone() {
    let secret = "github-webhook-secret";
    let (db, server) = database_and_github_app_server_with_task(true, secret).await;

    send_installation_created_event(&server, secret).await;

    assert_eq!(db.rows::<git_host_connection::Entity>().await.len(), 1);
    let task = db.find::<task::Entity>(task_id()).await.unwrap();
    assert_eq!(task.git_host_connection, None);
    assert!(!task.report_status);
    db.finish().await;
}

#[tokio::test]
async fn an_installation_event_for_a_declared_installation_links_no_task_and_keeps_it_declared() {
    let secret = "github-webhook-secret";
    let (db, server) = database_and_github_app_server_with_task(false, secret).await;
    let declared = gradient_db::rows::insert::<git_host_connection::Entity, _>(
        &db.connection(),
        git_host_connection::Model {
            id: GitHostConnectionId::now_v7(),
            name: "github".into(),
            display_name: "GitHub".into(),
            git_host_type: GitHostType::GitHub,
            receives_events: true,
            reports_status: true,
            github_installation_id: Some(9999),
            account_login: Some("old-name".into()),
            created_by: user_id(),
            created_at: test_date(),
            managed: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    send_installation_created_event(&server, secret).await;

    let rows = db.rows::<git_host_connection::Entity>().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, declared.id);
    assert!(rows[0].managed);
    assert_eq!(rows[0].account_login.as_deref(), Some("gh-org"));
    let task = db.find::<task::Entity>(task_id()).await.unwrap();
    assert_eq!(task.git_host_connection, None);
    db.finish().await;
}
