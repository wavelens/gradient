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
use gradient_ci::actions::encrypt_secret_with_file as encrypt_webhook_secret;
use gradient_core::ServerState;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::mock::{all_statements, empty_db, exec_ok, mock_db};
use gradient_fixtures::rows::worker_registration;
use gradient_test_support::state::TestState;
use gradient_types::GitHostType;
use gradient_types::ids::*;
use gradient_types::triggers::{ChangedFilePathFilter, ConcurrencyPolicy, TriggerConfig};
use gradient_web::create_router;
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{MockDatabase, MockExecResult};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn temp_secret_file(content: &str) -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, content).expect("write temp secret file");
    path.to_string_lossy().into_owned()
}

fn gitea_signature(secret: &str, body: &[u8]) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", gitea_signature(secret, body))
}

fn make_state(
    db: sea_orm::DatabaseConnection,
    crypt_path: Option<String>,
    gh_secret_path: Option<String>,
) -> Arc<ServerState> {
    TestState::new()
        .web_db(db)
        .cli(|cli| {
            if let Some(path) = crypt_path {
                cli.secrets.crypt_file = path;
            }
            if let Some(path) = gh_secret_path {
                cli.github_app.id = std::num::NonZeroU64::new(1234);
                cli.github_app.private_key_file = Some("/dev/null".into());
                cli.github_app.webhook_secret_file = Some(path);
            }
        })
        .build()
}

fn fixture_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000001").unwrap())
}
fn fixed_gitea_git_host_connection_id_for_webhook_tests() -> GitHostConnectionId {
    GitHostConnectionId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000002").unwrap())
}
fn task_id() -> TaskId {
    TaskId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000003").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000004").unwrap())
}
fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000005").unwrap())
}
fn commit_id() -> CommitId {
    CommitId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000006").unwrap())
}
fn trigger_id() -> TaskTriggerId {
    TaskTriggerId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000007").unwrap())
}

const GITEA_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

const GITEA_PUSH_BRANCH_BODY: &str = r#"{
    "ref": "refs/heads/feature/new-thing",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

// Gitea's "Test Delivery" button and real branch deletions are sending a push with an all-zero
// `after` SHA (#428).
const GITEA_TEST_WEBHOOK_BODY: &str = r#"{
    "ref": "refs/heads/master",
    "before": "0000000000000000000000000000000000000000",
    "after": "0000000000000000000000000000000000000000",
    "commits": [
        { "id": "0000000000000000000000000000000000000000", "message": "This is a fake commit", "verification": null, "added": null, "removed": null, "modified": null }
    ],
    "head_commit": { "id": "0000000000000000000000000000000000000000", "message": "This is a fake commit", "verification": null },
    "repository": {
        "clone_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

const GITHUB_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://github.com/gh-org/repo",
        "ssh_url": "git@github.com:gh-org/repo.git"
    },
    "installation": { "id": 9999 }
}"#;

const GITLAB_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "project": {
        "http_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

fn project_row(name: &str) -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id: project_id(),
        name: name.to_string(),
        display_name: "Test Project".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: fixture_date(),
        ..Default::default()
    }
}

fn test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(
    secret_ciphertext: &str,
) -> gradient_entity::git_host_connection::Model {
    gradient_entity::git_host_connection::Model {
        id: fixed_gitea_git_host_connection_id_for_webhook_tests(),
        project: Some(project_id()),
        name: "my-hook".into(),
        display_name: "my-hook".into(),
        base_url: Some("https://gitea.example.com".into()),
        secret: Some(secret_ciphertext.to_string()),
        receives_events: true,
        created_by: user_id(),
        created_at: fixture_date(),
        ..Default::default()
    }
}

fn build_instance_wide_git_host_connection_row_for_github_app_installation()
-> gradient_entity::git_host_connection::Model {
    gradient_entity::git_host_connection::Model {
        id: fixed_gitea_git_host_connection_id_for_webhook_tests(),
        name: "github-gh-project".into(),
        display_name: "GitHub".into(),
        git_host_type: GitHostType::GitHub,
        receives_events: true,
        reports_status: true,
        github_installation_id: Some(9999),
        created_by: user_id(),
        created_at: fixture_date(),
        ..Default::default()
    }
}

fn task_row() -> gradient_entity::task::Model {
    task_row_with(
        task_id(),
        project_id(),
        "test-task",
        "https://gitea.example.com/test-org/repo",
    )
}

fn task_row_with(
    id: TaskId,
    project: ProjectId,
    name: &str,
    repository: &str,
) -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        id,
        project,
        name: name.into(),
        active: true,
        display_name: "Test Task".into(),
        repository: repository.into(),
        wildcard: "*".into(),
        last_check_at: fixture_date(),
        created_by: user_id(),
        created_at: fixture_date(),
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::Skip,
        ..Default::default()
    }
}

fn github_task_row() -> gradient_entity::task::Model {
    task_row_with(
        task_id(),
        project_id(),
        "test-task",
        "https://github.com/gh-org/repo",
    )
}

fn eval_row(status: EvaluationStatus) -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id: eval_id(),
        task: Some(task_id()),
        repository: "https://gitea.example.com/test-org/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status,
        created_at: fixture_date(),
        updated_at: fixture_date(),
        ..Default::default()
    }
}

fn commit_row() -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id: commit_id(),
        hash: vec![0u8; 20],
        ..Default::default()
    }
}

fn cache_row() -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: CacheId::now_v7(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 10,
        created_by: UserId::nil(),
        created_at: fixture_date(),
        ..Default::default()
    }
}

fn project_cache_row() -> gradient_entity::project_cache::Model {
    gradient_entity::project_cache::Model {
        id: ProjectCacheId::now_v7(),
        project: project_id(),
        cache: CacheId::now_v7(),
        mode: gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
        ..Default::default()
    }
}

fn worker_registration_row() -> gradient_entity::worker_registration::Model {
    gradient_entity::worker_registration::Model {
        peer_id: project_id(),
        created_at: fixture_date(),
        ..worker_registration(true, true)
    }
}

fn trigger_row(cfg: TriggerConfig) -> gradient_entity::task_trigger::Model {
    gradient_entity::task_trigger::Model {
        id: trigger_id(),
        task: task_id(),
        trigger_type: cfg.trigger_type(),
        config: cfg.to_db_json(),
        active: true,
        created_at: fixture_date(),
        updated_at: fixture_date(),
        ..Default::default()
    }
}

fn reporter_push_trigger(branches: Vec<&str>) -> TriggerConfig {
    TriggerConfig::ReporterPush {
        branches: branches.into_iter().map(String::from).collect(),
        tags: vec![],
        releases_only: false,
        include_flake_inputs: false,
        path_filter: ChangedFilePathFilter::default(),
    }
}

fn reporter_push_releases_only_trigger() -> TriggerConfig {
    TriggerConfig::ReporterPush {
        branches: vec![],
        tags: vec![],
        releases_only: true,
        include_flake_inputs: false,
        path_filter: ChangedFilePathFilter::default(),
    }
}

fn reporter_pr_trigger(actions: Vec<&str>) -> TriggerConfig {
    TriggerConfig::ReporterPullRequest {
        branches: vec![],
        actions: actions.into_iter().map(String::from).collect(),
        require_approval: false,
        path_filter: ChangedFilePathFilter::default(),
    }
}

fn example_github_repository_grant_row_for_gh_org_all_repositories()
-> gradient_entity::git_host_connection_repository_grant::Model {
    gradient_entity::git_host_connection_repository_grant::Model {
        id: GitHostConnectionRepositoryGrantId::now_v7(),
        git_host_connection: fixed_gitea_git_host_connection_id_for_webhook_tests(),
        project: project_id(),
        repository_owner: "gh-org".into(),
        repository_name: None,
        created_at: fixture_date(),
        ..Default::default()
    }
}

trait AppendGitHostRepositoryAccessQueryResults {
    fn append_mocked_rows_for_repository_grant_check(
        self,
        connection: gradient_entity::git_host_connection::Model,
        task: gradient_entity::task::Model,
        grants: Vec<gradient_entity::git_host_connection_repository_grant::Model>,
    ) -> Self;
}

impl AppendGitHostRepositoryAccessQueryResults for MockDatabase {
    fn append_mocked_rows_for_repository_grant_check(
        self,
        connection: gradient_entity::git_host_connection::Model,
        task: gradient_entity::task::Model,
        grants: Vec<gradient_entity::git_host_connection_repository_grant::Model>,
    ) -> Self {
        self.append_query_results([vec![connection]])
            .append_query_results([vec![task]])
            .append_query_results([vec![project_row("test-project")]])
            .append_query_results([grants])
    }
}

fn apply_trigger_db_chain(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![task_row()]])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![project_cache_row()]])
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![worker_registration_row()]])
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_row(EvaluationStatus::Queued)]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
}

#[tokio::test]
async fn git_host_webhook_no_matching_trigger() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn git_host_webhook_push_fires_trigger() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["tasks_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["task_name"], "test-task");
    assert_eq!(queued[0]["project"], "test-project");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn git_host_webhook_test_ping_zero_sha_is_ok_noop() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_TEST_WEBHOOK_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn git_host_webhook_invalid_signature() {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "correct-secret").expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let wrong_sig = gitea_signature("wrong-secret", body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &wrong_sig)
        .bytes(body.into())
        .await;

    response.assert_status_unauthorized();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "invalid webhook signature");
}

#[tokio::test]
async fn gitea_old_webhook_path_with_unknown_connection_name_returns_404() {
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([Vec::<gradient_entity::git_host_connection::Model>::new()])
        .into_connection();

    let state = make_state(
        db,
        Some(temp_secret_file("any-32-byte-secret-here!!!!!!!!")),
        None,
    );
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/missing-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", "doesnotmatter")
        .bytes(GITEA_PUSH_BODY.as_bytes().into())
        .await;

    response.assert_status_not_found();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(
        json["message"],
        "No Git host connection at this address. No events reach that connection."
    );
}

#[tokio::test]
async fn git_host_webhook_without_secret_names_the_missing_secret() {
    let secretless = gradient_entity::git_host_connection::Model {
        secret: None,
        ..test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret("unused")
    };
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![secretless]])
        .into_connection();

    let state = make_state(
        db,
        Some(temp_secret_file("any-32-byte-secret-here!!!!!!!!")),
        None,
    );
    let server = TestServer::new(create_router(state).expect("router"));

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", "doesnotmatter")
        .bytes(GITEA_PUSH_BODY.as_bytes().into())
        .await;

    response.assert_status_unauthorized();
    let json: Value = response.json();
    assert!(
        json["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("no webhook secret"),
        "{json}"
    );
}

#[tokio::test]
async fn refused_delivery_without_secret_is_logged_for_connection() {
    let secretless = gradient_entity::git_host_connection::Model {
        secret: None,
        ..test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret("unused")
    };
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![secretless]])
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
        ])
        .into_connection();
    let log = db.clone();

    let state = make_state(
        db,
        Some(temp_secret_file("any-32-byte-secret-here!!!!!!!!")),
        None,
    );
    let server = TestServer::new(create_router(state).expect("router"));
    server
        .post("/api/v1/hooks/forgejo/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", "doesnotmatter")
        .bytes(GITEA_PUSH_BODY.as_bytes().into())
        .await
        .assert_status_unauthorized();
    drop(server);

    let statements = format!("{:?}", log.into_transaction_log());
    assert!(
        statements.contains("incoming_webhook_delivery"),
        "{statements}"
    );
    assert!(
        statements.to_lowercase().contains("no webhook secret"),
        "{statements}"
    );
}

#[tokio::test]
async fn a_delivery_with_an_invalid_signature_is_not_logged() {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "correct-secret").expect("encrypt");
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
        ])
        .into_connection();
    let log = db.clone();

    let server =
        TestServer::new(create_router(make_state(db, Some(crypt_path), None)).expect("router"));
    let body = GITEA_PUSH_BODY.as_bytes();
    server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", gitea_signature("wrong-secret", body))
        .bytes(body.into())
        .await
        .assert_status_unauthorized();
    drop(server);

    let statements = format!("{:?}", log.into_transaction_log());
    assert!(
        !statements.contains("incoming_webhook_delivery"),
        "{statements}"
    );
}

async fn post_webhook_to_legacy_address_and_get_status_and_executed_statements(
    connection: gradient_entity::git_host_connection::Model,
    crypt_path: String,
    address: &str,
    headers: [(&str, String); 2],
    body: &'static str,
) -> (u16, String) {
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![connection]])
        .append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()])
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
        ])
        .into_connection();
    let log = db.clone();

    let server =
        TestServer::new(create_router(make_state(db, Some(crypt_path), None)).expect("router"));
    let mut request = server.post(address).bytes(body.as_bytes().into());
    for (name, value) in headers {
        request = request.add_header(name, value);
    }
    let status = request.await.status_code().as_u16();
    drop(server);

    (status, format!("{:?}", log.into_transaction_log()))
}

#[tokio::test]
async fn legacy_webhook_with_mismatched_git_host_returns_not_found_and_starts_no_evaluation() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let (status, statements) =
        post_webhook_to_legacy_address_and_get_status_and_executed_statements(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            crypt_path,
            "/api/v1/hooks/gitlab/test-project/my-hook",
            [
                ("X-Gitlab-Event", "Push Hook".to_owned()),
                ("X-Gitlab-Token", plaintext_secret.to_owned()),
            ],
            GITLAB_PUSH_BODY,
        )
        .await;

    assert_eq!(status, 404, "{statements}");
    assert!(!statements.contains("task_trigger"), "{statements}");
    assert!(
        !statements.contains("incoming_webhook_delivery"),
        "{statements}"
    );
}

#[tokio::test]
async fn github_app_installation_connection_old_webhook_address_returns_not_found() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");
    let connection = gradient_entity::git_host_connection::Model {
        project: Some(project_id()),
        name: "my-hook".into(),
        secret: Some(ciphertext),
        ..build_instance_wide_git_host_connection_row_for_github_app_installation()
    };
    let body = GITHUB_PUSH_BODY;

    let (status, statements) =
        post_webhook_to_legacy_address_and_get_status_and_executed_statements(
            connection,
            crypt_path,
            "/api/v1/hooks/github/test-project/my-hook",
            [
                ("X-GitHub-Event", "push".to_owned()),
                (
                    "X-Hub-Signature-256",
                    github_signature(plaintext_secret, body.as_bytes()),
                ),
            ],
            body,
        )
        .await;

    assert_eq!(status, 404, "{statements}");
    assert!(!statements.contains("task_trigger"), "{statements}");
}

async fn post_signed_gitea_push_to_legacy_address_with_git_host_path_for_connection_without_triggers_and_get_status_and_json_body(
    connection_git_host_type: GitHostType,
    git_host_path_segment: &str,
    signing_secret: &str,
) -> (u16, Value) {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "test-secret-plaintext").expect("encrypt");
    let connection = gradient_entity::git_host_connection::Model {
        git_host_type: connection_git_host_type,
        ..test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext)
    };
    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![connection]])
        .append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()])
        .into_connection();
    let server =
        TestServer::new(create_router(make_state(db, Some(crypt_path), None)).expect("router"));

    let body = GITEA_PUSH_BODY.as_bytes();
    let response = server
        .post(&format!(
            "/api/v1/hooks/{git_host_path_segment}/test-project/my-hook"
        ))
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", gitea_signature(signing_secret, body))
        .bytes(body.into())
        .await;

    (response.status_code().as_u16(), response.json())
}

#[tokio::test]
async fn push_to_old_webhook_url_with_forgejo_path_for_gitea_connection_with_connection_secret_signature_is_accepted()
 {
    let (status, json) =
        post_signed_gitea_push_to_legacy_address_with_git_host_path_for_connection_without_triggers_and_get_status_and_json_body(
            GitHostType::Gitea,
            "forgejo",
            "test-secret-plaintext",
        )
        .await;

    assert_eq!(status, 200, "{json}");
    assert_eq!(json["error"], false);
    assert_eq!(json["message"]["event"], "push");
    assert_eq!(json["message"]["tasks_scanned"], 0);
}

#[tokio::test]
async fn push_to_old_webhook_url_with_gitea_path_for_forgejo_connection_returns_200_and_handles_push_event()
 {
    let (status, json) =
        post_signed_gitea_push_to_legacy_address_with_git_host_path_for_connection_without_triggers_and_get_status_and_json_body(
            GitHostType::Forgejo,
            "gitea",
            "test-secret-plaintext",
        )
        .await;

    assert_eq!(status, 200, "{json}");
    assert_eq!(json["error"], false);
    assert_eq!(json["message"]["event"], "push");
    assert_eq!(json["message"]["tasks_scanned"], 0);
}

#[tokio::test]
async fn push_with_wrong_signature_to_old_forgejo_webhook_url_for_gitea_connection_returns_401_invalid_webhook_signature()
 {
    let (status, json) =
        post_signed_gitea_push_to_legacy_address_with_git_host_path_for_connection_without_triggers_and_get_status_and_json_body(
            GitHostType::Gitea,
            "forgejo",
            "wrong-secret",
        )
        .await;

    assert_eq!(status, 401, "{json}");
    assert_eq!(json["message"], "invalid webhook signature");
}

#[tokio::test]
async fn git_host_webhook_branch_glob_no_match_skipped() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec!["release/*"]))]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BRANCH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());

    let skipped = msg["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["reason"], "filter");
    assert_eq!(
        skipped[0]["reason_text"],
        "Branch not in trigger's branch list"
    );
}

#[tokio::test]
async fn git_host_webhook_pr_fires_trigger() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let pr_body = format!(
        r#"{{
            "action": "opened",
            "pull_request": {{
                "head": {{
                    "sha": "{VALID_SHA}",
                    "ref": "feature-x",
                    "name": "feature-x"
                }}
            }},
            "repository": {{
                "clone_url": "https://gitea.example.com/test-org/repo",
                "ssh_url": "git@gitea.example.com:test-org/repo.git"
            }}
        }}"#
    );

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec![
            "opened",
            "synchronize",
        ]))]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = pr_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "pull_request")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "pull_request");
    assert_eq!(msg["tasks_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["task_name"], "test-task");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

async fn send_gitea_fork_pull_request_webhook_and_return_evaluation_insert_sql(
    fork_clone_url: &str,
) -> String {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let pr_body = serde_json::json!({
        "action": "opened",
        "number": 7,
        "pull_request": {
            "head": {
                "sha": VALID_SHA,
                "ref": "feature-x",
                "label": "feature-x",
                "repo": { "full_name": "fork-owner/repo", "clone_url": fork_clone_url }
            },
            "base": { "sha": VALID_SHA, "ref": "main", "repo": { "full_name": "test-org/repo" } }
        },
        "repository": {
            "full_name": "test-org/repo",
            "clone_url": "https://gitea.example.com/test-org/repo",
            "ssh_url": "git@gitea.example.com:test-org/repo.git"
        }
    })
    .to_string();

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec!["opened"]))]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db.clone(), Some(crypt_path), None);
    let server = TestServer::new(create_router(state).expect("router"));
    let body_bytes = pr_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);
    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "pull_request")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;
    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["message"]["queued"].as_array().unwrap().len(), 1);

    gradient_fixtures::mock::all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.starts_with(r#"INSERT INTO "evaluation""#))
        .map(|s| format!("{s:?}"))
        .expect("the delivery inserts an evaluation")
}

#[tokio::test]
async fn fork_pull_request_on_another_host_is_evaluated_from_task_repository_not_fork_clone_url() {
    let insert = send_gitea_fork_pull_request_webhook_and_return_evaluation_insert_sql(
        "https://other-host.example.org/fork-owner/repo",
    )
    .await;
    assert!(!insert.contains("other-host.example.org"), "{insert}");
    assert!(
        insert.contains("gitea.example.com/test-org/repo"),
        "{insert}"
    );
}

#[tokio::test]
async fn gitea_fork_pull_request_with_fork_on_task_host_uses_fork_clone_url() {
    let insert = send_gitea_fork_pull_request_webhook_and_return_evaluation_insert_sql(
        "https://gitea.example.com/fork-owner/repo",
    )
    .await;
    assert!(
        insert.contains("gitea.example.com/fork-owner/repo"),
        "{insert}"
    );
}

#[tokio::test]
async fn git_host_webhook_pr_action_mismatch_skipped() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let pr_body = format!(
        r#"{{
            "action": "closed",
            "pull_request": {{
                "head": {{
                    "sha": "{VALID_SHA}",
                    "name": "feature-x"
                }}
            }},
            "repository": {{
                "clone_url": "https://gitea.example.com/test-org/repo"
            }}
        }}"#
    );

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec!["opened"]))]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = pr_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "pull_request")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "pull_request");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());

    let skipped = msg["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["reason"], "filter");
}

#[tokio::test]
async fn git_host_webhook_release_fires_releases_only_trigger() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let release_body = format!(
        r#"{{
            "action": "published",
            "release": {{
                "tag_name": "v1.0.0",
                "sha": "{VALID_SHA}",
                "target_commitish": "main"
            }},
            "repository": {{
                "clone_url": "https://gitea.example.com/test-org/repo",
                "ssh_url": "git@gitea.example.com:test-org/repo.git"
            }}
        }}"#
    );

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![project_row("test-project")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = release_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "release")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "release");
    assert_eq!(msg["tasks_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["task_name"], "test-task");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn git_host_webhook_push_does_not_fire_releases_only_trigger() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = mock_db()
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
        .append_mocked_rows_for_repository_grant_check(
            test_project_gitea_connection_row_for_webhook_events_with_encrypted_secret(&ciphertext),
            task_row(),
            vec![],
        )
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-project/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn github_app_webhook_push_fires_trigger() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let db = mock_db()
        .append_query_results([vec![
            build_instance_wide_git_host_connection_row_for_github_app_installation(),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_mocked_rows_for_repository_grant_check(
            build_instance_wide_git_host_connection_row_for_github_app_installation(),
            github_task_row(),
            vec![example_github_repository_grant_row_for_gh_org_all_repositories()],
        )
        .append_query_results([vec![github_task_row()]])
        .append_query_results([vec![project_row("gh-project")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITHUB_PUSH_BODY.as_bytes();
    let sig = github_signature(gh_secret, body);

    let response = server
        .post("/api/v1/hooks/github")
        .add_header("X-Hub-Signature-256", &sig)
        .add_header("X-GitHub-Event", "push")
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["tasks_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["task_name"], "test-task");
    assert_eq!(queued[0]["project"], "gh-project");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn github_app_webhook_ping() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let db = empty_db();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body: &[u8] = b"{}";
    let sig = github_signature(gh_secret, body);

    let response = server
        .post("/api/v1/hooks/github")
        .add_header("X-Hub-Signature-256", &sig)
        .add_header("X-GitHub-Event", "ping")
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "ping");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn github_app_webhook_installation() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let db = empty_db();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = serde_json::to_vec(&serde_json::json!({
        "action": "created",
        "installation": {
            "id": 9999,
            "account": { "login": "gh-project" }
        },
        "sender": { "login": "some-user" }
    }))
    .unwrap();
    let sig = github_signature(gh_secret, &body);

    let response = server
        .post("/api/v1/hooks/github")
        .add_header("X-Hub-Signature-256", &sig)
        .add_header("X-GitHub-Event", "installation")
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "installation");
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn github_app_webhook_not_configured() {
    let db = empty_db();

    let state = make_state(db, None, None);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let response = server
        .post("/api/v1/hooks/github")
        .add_header("X-Hub-Signature-256", "sha256=doesnotmatter")
        .add_header("X-GitHub-Event", "push")
        .bytes(axum::body::Bytes::from_static(b"{}"))
        .await;

    response.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "No GitHub App configured");
}

#[tokio::test]
async fn github_app_webhook_no_matching_repo_returns_zero() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let unrelated = task_row_with(
        task_id(),
        project_id(),
        "unrelated",
        "https://github.com/somewhere/else",
    );

    let db = mock_db()
        .append_query_results([vec![
            build_instance_wide_git_host_connection_row_for_github_app_installation(),
        ]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_mocked_rows_for_repository_grant_check(
            build_instance_wide_git_host_connection_row_for_github_app_installation(),
            unrelated.clone(),
            vec![
                gradient_entity::git_host_connection_repository_grant::Model {
                    repository_owner: "somewhere".into(),
                    ..example_github_repository_grant_row_for_gh_org_all_repositories()
                },
            ],
        )
        .append_query_results([vec![unrelated]])
        .append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()])
        .into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = GITHUB_PUSH_BODY.as_bytes();
    let sig = github_signature(gh_secret, body);

    let response = server
        .post("/api/v1/hooks/github")
        .add_header("X-Hub-Signature-256", &sig)
        .add_header("X-GitHub-Event", "push")
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    let msg = &json["message"];
    assert_eq!(msg["tasks_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

fn active_post_request_trigger_row_with_encrypted_secret(
    encrypted_secret: Option<String>,
) -> gradient_entity::task_trigger::Model {
    gradient_entity::task_trigger::Model {
        encrypted_secret,
        last_fired_at: Some(fixture_date()),
        ..trigger_row(TriggerConfig::PostRequest {
            branch: None,
            path_filter: Default::default(),
        })
    }
}

fn trigger_webhook_path_for_test_trigger_id() -> String {
    format!("/api/v1/hooks/trigger/{}", trigger_id())
}

#[tokio::test]
async fn post_trigger_hook_url_returns_404_for_missing_inactive_wrong_type_trigger_or_inactive_task()
 {
    let post_request = active_post_request_trigger_row_with_encrypted_secret(Some("x".into()));
    let inactive_trigger = gradient_entity::task_trigger::Model {
        active: false,
        ..post_request.clone()
    };
    let polling_trigger = trigger_row(TriggerConfig::Polling {
        interval_secs: 60,
        branch: None,
        path_filter: Default::default(),
    });
    let inactive_task = gradient_entity::task::Model {
        active: false,
        ..task_row()
    };

    let cases = [
        mock_db().append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()]),
        mock_db()
            .append_query_results([vec![inactive_trigger]])
            .append_query_results([vec![task_row()]]),
        mock_db()
            .append_query_results([vec![polling_trigger]])
            .append_query_results([vec![task_row()]]),
        mock_db()
            .append_query_results([vec![post_request]])
            .append_query_results([vec![inactive_task]]),
    ];

    for db in cases {
        let server = TestServer::new(
            create_router(make_state(db.into_connection(), None, None)).expect("router"),
        );
        let response = server
            .post(&trigger_webhook_path_for_test_trigger_id())
            .add_header("Authorization", "Bearer x")
            .await;
        response.assert_status_not_found();
    }
}

#[tokio::test]
async fn post_trigger_hook_url_returns_401_when_trigger_has_no_stored_secret() {
    let db = mock_db()
        .append_query_results([vec![active_post_request_trigger_row_with_encrypted_secret(
            None,
        )]])
        .append_query_results([vec![task_row()]])
        .into_connection();
    let server = TestServer::new(create_router(make_state(db, None, None)).expect("router"));

    let response = server
        .post(&trigger_webhook_path_for_test_trigger_id())
        .add_header("Authorization", "Bearer ")
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn post_trigger_hook_url_returns_401_when_stored_secret_is_empty_string() {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "").expect("encrypt");
    let db = mock_db()
        .append_query_results([vec![active_post_request_trigger_row_with_encrypted_secret(
            Some(ciphertext),
        )]])
        .append_query_results([vec![task_row()]])
        .into_connection();
    let server =
        TestServer::new(create_router(make_state(db, Some(crypt_path), None)).expect("router"));

    let response = server
        .post(&trigger_webhook_path_for_test_trigger_id())
        .add_header("X-Gitlab-Token", "")
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn post_trigger_hook_with_wrong_signature_returns_unauthorized() {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "right-secret").expect("encrypt");
    let db = mock_db()
        .append_query_results([vec![active_post_request_trigger_row_with_encrypted_secret(
            Some(ciphertext),
        )]])
        .append_query_results([vec![task_row()]])
        .into_connection();
    let server =
        TestServer::new(create_router(make_state(db, Some(crypt_path), None)).expect("router"));
    let body = GITHUB_PUSH_BODY.as_bytes();

    let response = server
        .post(&trigger_webhook_path_for_test_trigger_id())
        .add_header(
            "X-Hub-Signature-256",
            &github_signature("other-secret", body),
        )
        .bytes(body.into())
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn valid_bearer_post_enqueues_one_trigger_fire_delivery_with_row_last_fired_at_and_returns_202()
 {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "s3cret").expect("encrypt");
    let trigger = active_post_request_trigger_row_with_encrypted_secret(Some(ciphertext));
    let web_db = mock_db()
        .append_query_results([vec![trigger.clone()]])
        .append_query_results([vec![task_row()]])
        .append_query_results([vec![trigger]])
        .append_query_results([vec![task_row()]])
        .into_connection();
    let worker_db = mock_db()
        .append_exec_results([exec_ok(), exec_ok()])
        .into_connection();
    let server = TestState::new()
        .web_db(web_db)
        .worker_db(worker_db.clone())
        .cli(|cli| cli.secrets.crypt_file = crypt_path)
        .server();

    for _ in 0..2 {
        let response = server
            .post(&trigger_webhook_path_for_test_trigger_id())
            .add_header("Authorization", "Bearer s3cret")
            .await;
        response.assert_status(axum::http::StatusCode::ACCEPTED);
        let json: Value = response.json();
        assert_eq!(json["message"]["trigger"], trigger_id().to_string());
    }

    let expected_key = format!("{}:{}", trigger_id(), fixture_date());
    let inserts: Vec<_> = all_statements(worker_db.into_transaction_log())
        .into_iter()
        .filter(|s| s.sql.starts_with("INSERT INTO pending_delivery"))
        .collect();
    assert_eq!(inserts.len(), 2);
    for insert in inserts {
        let values = format!("{:?}", insert.values);
        assert!(values.contains(&expected_key), "{values}");
    }
}

const GITEA_WEBHOOK_PLAINTEXT_SECRET_FOR_FORK_PULL_REQUEST_APPROVAL_TESTS: &str =
    "gitea-webhook-secret";
const FORK_PULL_REQUEST_APPROVAL_TEST_GITHUB_APP_WEBHOOK_SECRET: &str = "github-webhook-secret";
const PENDING_FORK_PULL_REQUEST_APPROVAL_CHECK_RUN_ID: i64 = 4242;
const FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER_FORK_COMMIT_HASH_WITH_NO_PENDING_APPROVAL: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_REVIEWER_USER_ID: i64 = 42;
const TEST_REVIEW_WEBHOOK_SENDER_WITHOUT_PUSH_PERMISSION_USER_ID: i64 = 43;

struct ForkPullRequestApprovalTestFixture {
    db: TestDatabase,
    server: TestServer,
    git_host: MockServer,
    connection: GitHostConnectionId,
    pending_approval: PullRequestApprovalId,
}

async fn create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
-> ForkPullRequestApprovalTestFixture {
    let git_host = MockServer::start().await;
    let db = TestDatabase::create().await;
    let conn = db.connection();
    let crypt = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let github_app_webhook_secret_file =
        temp_secret_file(FORK_PULL_REQUEST_APPROVAL_TEST_GITHUB_APP_WEBHOOK_SECRET);
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(
        &conn,
        gradient_fixtures::rows::user(),
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<gradient_entity::project::Entity, _>(
        &conn,
        gradient_fixtures::rows::project(),
    )
    .await
    .unwrap();
    let connection = gradient_db::rows::insert::<gradient_entity::git_host_connection::Entity, _>(
        &conn,
        gradient_entity::git_host_connection::Model {
            id: GitHostConnectionId::now_v7(),
            project: Some(gradient_fixtures::ids::project_id()),
            name: "gitea".into(),
            display_name: "Gitea".into(),
            git_host_type: GitHostType::Gitea,
            base_url: Some(git_host.uri()),
            secret: Some(
                encrypt_webhook_secret(
                    &crypt,
                    GITEA_WEBHOOK_PLAINTEXT_SECRET_FOR_FORK_PULL_REQUEST_APPROVAL_TESTS,
                )
                .unwrap(),
            ),
            access_token: Some(encrypt_webhook_secret(&crypt, "token").unwrap()),
            receives_events: true,
            reports_status: true,
            created_by: gradient_fixtures::ids::user_id(),
            created_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<gradient_entity::task::Entity, _>(
        &conn,
        gradient_entity::task::Model {
            repository: format!("{}/test-org/repo", git_host.uri()),
            git_host_connection: Some(connection.id),
            report_status: true,
            ..gradient_fixtures::rows::task()
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
        gradient_entity::task_trigger::Model {
            id: TaskTriggerId::now_v7(),
            task: gradient_fixtures::ids::task_id(),
            trigger_type: pull_request.trigger_type(),
            config: pull_request.to_db_json(),
            active: true,
            created_at: gradient_fixtures::test_date(),
            updated_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pending_approval =
        gradient_db::rows::insert::<gradient_entity::fork_pull_request_approval::Entity, _>(
            &conn,
            gradient_entity::fork_pull_request_approval::Model {
                id: PullRequestApprovalId::now_v7(),
                task: gradient_fixtures::ids::task_id(),
                trigger: trigger.id,
                pr_number: 7,
                pr_author: "external".into(),
                commit_hash: hex::decode(
                    FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
                )
                .unwrap(),
                check_run_id: Some(PENDING_FORK_PULL_REQUEST_APPROVAL_CHECK_RUN_ID),
                head_repo_clone_url: Some(format!("{}/external/repo", git_host.uri())),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(|cli| {
            cli.secrets.crypt_file = crypt;
            cli.github_app.id = std::num::NonZeroU64::new(1234);
            cli.github_app.private_key_file = Some("/dev/null".into());
            cli.github_app.webhook_secret_file = Some(github_app_webhook_secret_file);
        })
        .server();
    ForkPullRequestApprovalTestFixture {
        db,
        server,
        git_host,
        connection: connection.id,
        pending_approval: pending_approval.id,
    }
}

async fn mount_collaborator_permission_endpoint_mock(
    git_host: &MockServer,
    repository_full_name: &str,
    login: &str,
    permission: &str,
    expected_calls: u64,
) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/repos/{repository_full_name}/collaborators/{login}/permission"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "permission": permission })),
        )
        .expect(expected_calls)
        .mount(git_host)
        .await;
}

async fn mount_gitea_pull_request_reviews_list_mock_with_expected_call_count(
    git_host: &MockServer,
    reviews: Value,
    expected_calls: u64,
) {
    for (page, body) in [("1", reviews), ("2", serde_json::json!([]))] {
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/test-org/repo/pulls/7/reviews"))
            .and(query_param("page", page))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(expected_calls)
            .mount(git_host)
            .await;
    }
}

fn build_undismissed_approved_pull_request_review_json(
    user_id: i64,
    login: &str,
    commit_hash: &str,
) -> Value {
    serde_json::json!({
        "id": 1,
        "user": { "id": user_id, "login": login },
        "state": "APPROVED",
        "commit_id": commit_hash,
        "dismissed": false
    })
}

fn build_gitea_approved_pull_request_review_webhook_body(
    reviewer: &str,
    sender_user_id: i64,
    commit_hash: &str,
) -> Vec<u8> {
    serde_json::json!({
        "action": "reviewed",
        "review": { "type": "pull_request_review_approved", "content": "" },
        "commit_id": commit_hash,
        "pull_request": { "number": 7 },
        "repository": { "clone_url": "x", "full_name": "test-org/repo" },
        "sender": { "id": sender_user_id, "username": reviewer }
    })
    .to_string()
    .into_bytes()
}

fn build_check_run_requested_action_webhook_body(
    repository_full_name: &str,
    sender: &str,
) -> Vec<u8> {
    serde_json::json!({
        "action": "requested_action",
        "requested_action": { "identifier": gradient_ci::APPROVAL_ACTION_ID },
        "check_run": { "id": PENDING_FORK_PULL_REQUEST_APPROVAL_CHECK_RUN_ID },
        "repository": { "full_name": repository_full_name },
        "sender": { "login": sender }
    })
    .to_string()
    .into_bytes()
}

async fn post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
    fixture: &ForkPullRequestApprovalTestFixture,
    body: Vec<u8>,
) {
    fixture
        .server
        .post(&format!("/api/v1/hooks/{}", fixture.connection))
        .add_header("X-Gitea-Event", "pull_request_review")
        .add_header(
            "X-Gitea-Signature",
            gitea_signature(
                GITEA_WEBHOOK_PLAINTEXT_SECRET_FOR_FORK_PULL_REQUEST_APPROVAL_TESTS,
                &body,
            ),
        )
        .bytes(body.into())
        .await
        .assert_status_ok();
}

async fn post_signed_github_app_check_run_webhook_and_assert_http_200(
    fixture: &ForkPullRequestApprovalTestFixture,
    body: Vec<u8>,
) {
    fixture
        .server
        .post("/api/v1/hooks/github")
        .add_header("X-GitHub-Event", "check_run")
        .add_header(
            "X-Hub-Signature-256",
            github_signature(
                FORK_PULL_REQUEST_APPROVAL_TEST_GITHUB_APP_WEBHOOK_SECRET,
                &body,
            ),
        )
        .bytes(body.into())
        .await
        .assert_status_ok();
}

async fn assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(
    fixture: ForkPullRequestApprovalTestFixture,
) {
    assert!(
        fixture
            .db
            .rows::<gradient_entity::evaluation::Entity>()
            .await
            .is_empty()
    );
    let approval = fixture
        .db
        .find::<gradient_entity::fork_pull_request_approval::Entity>(fixture.pending_approval)
        .await
        .unwrap();
    assert_eq!(
        (approval.approved_at, approval.approving_maintainer_login),
        (None, None)
    );
    fixture.git_host.verify().await;
    fixture.db.finish().await;
}

#[tokio::test]
async fn gitea_review_webhook_without_commit_approves_pending_fork_commit_named_by_maintainer_newest_approving_review()
 {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    mount_collaborator_permission_endpoint_mock(
        &fixture.git_host,
        "test-org/repo",
        "maintainer",
        "write",
        1,
    )
    .await;
    mount_gitea_pull_request_reviews_list_mock_with_expected_call_count(
        &fixture.git_host,
        serde_json::json!([build_undismissed_approved_pull_request_review_json(
            TEST_REVIEWER_USER_ID,
            "maintainer",
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        )]),
        1,
    )
    .await;

    post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
        &fixture,
        build_gitea_approved_pull_request_review_webhook_body(
            "maintainer",
            TEST_REVIEWER_USER_ID,
            "",
        ),
    )
    .await;

    let approval = fixture
        .db
        .find::<gradient_entity::fork_pull_request_approval::Entity>(fixture.pending_approval)
        .await
        .unwrap();
    assert!(approval.approved_at.is_some());
    assert_eq!(
        approval.approving_maintainer_login.as_deref(),
        Some("maintainer")
    );
    fixture.git_host.verify().await;
    fixture.db.finish().await;
}

#[tokio::test]
async fn test_gitea_review_webhook_for_pending_commit_b_approves_nothing_when_api_says_maintainer_approving_review_is_for_commit_a()
 {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    mount_collaborator_permission_endpoint_mock(
        &fixture.git_host,
        "test-org/repo",
        "maintainer",
        "write",
        1,
    )
    .await;
    mount_gitea_pull_request_reviews_list_mock_with_expected_call_count(
        &fixture.git_host,
        serde_json::json!([build_undismissed_approved_pull_request_review_json(
            TEST_REVIEWER_USER_ID,
            "maintainer",
            OTHER_FORK_COMMIT_HASH_WITH_NO_PENDING_APPROVAL,
        )]),
        1,
    )
    .await;

    post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
        &fixture,
        build_gitea_approved_pull_request_review_webhook_body(
            "maintainer",
            TEST_REVIEWER_USER_ID,
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        ),
    )
    .await;

    assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(fixture).await;
}

#[tokio::test]
async fn gitea_review_webhook_reviews_api_500_keeps_fork_pull_request_approval_pending() {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    mount_collaborator_permission_endpoint_mock(
        &fixture.git_host,
        "test-org/repo",
        "maintainer",
        "write",
        1,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/test-org/repo/pulls/7/reviews"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&fixture.git_host)
        .await;

    post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
        &fixture,
        build_gitea_approved_pull_request_review_webhook_body(
            "maintainer",
            TEST_REVIEWER_USER_ID,
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        ),
    )
    .await;

    assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(fixture).await;
}

async fn assert_delayed_permission_and_reviews_answers_create_no_evaluation_and_keep_fork_pull_request_unapproved(
    permission_answer_delay: Duration,
    reviews_answer_delay: Duration,
) {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/repos/test-org/repo/collaborators/maintainer/permission",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "permission": "write" }))
                .set_delay(permission_answer_delay),
        )
        .expect(1)
        .mount(&fixture.git_host)
        .await;
    for (page, body) in [
        (
            "1",
            serde_json::json!([build_undismissed_approved_pull_request_review_json(
                TEST_REVIEWER_USER_ID,
                "maintainer",
                FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
            )]),
        ),
        ("2", serde_json::json!([])),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/test-org/repo/pulls/7/reviews"))
            .and(query_param("page", page))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .set_delay(reviews_answer_delay),
            )
            .mount(&fixture.git_host)
            .await;
    }

    post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
        &fixture,
        build_gitea_approved_pull_request_review_webhook_body(
            "maintainer",
            TEST_REVIEWER_USER_ID,
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        ),
    )
    .await;

    assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(fixture).await;
}

#[tokio::test]
async fn gitea_review_webhook_reviews_api_5_second_delay_keeps_fork_pull_request_approval_pending_without_starting_evaluation()
 {
    assert_delayed_permission_and_reviews_answers_create_no_evaluation_and_keep_fork_pull_request_unapproved(
        Duration::ZERO,
        Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn gitea_review_webhook_collaborator_permission_check_timeout_keeps_fork_pull_request_approval_pending()
 {
    assert_delayed_permission_and_reviews_answers_create_no_evaluation_and_keep_fork_pull_request_unapproved(
        Duration::from_secs(5),
        Duration::ZERO,
    )
    .await;
}

#[tokio::test]
async fn approvals_from_user_without_push_permission_stay_pending() {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    mount_collaborator_permission_endpoint_mock(
        &fixture.git_host,
        "test-org/repo",
        "outsider",
        "read",
        2,
    )
    .await;
    mount_gitea_pull_request_reviews_list_mock_with_expected_call_count(
        &fixture.git_host,
        serde_json::json!([build_undismissed_approved_pull_request_review_json(
            TEST_REVIEW_WEBHOOK_SENDER_WITHOUT_PUSH_PERMISSION_USER_ID,
            "outsider",
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        )]),
        0,
    )
    .await;

    post_signed_gitea_pull_request_review_webhook_to_connection_url_and_assert_http_200(
        &fixture,
        build_gitea_approved_pull_request_review_webhook_body(
            "outsider",
            TEST_REVIEW_WEBHOOK_SENDER_WITHOUT_PUSH_PERMISSION_USER_ID,
            FORK_PULL_REQUEST_COMMIT_HASH_WITH_PENDING_MAINTAINER_APPROVAL,
        ),
    )
    .await;
    post_signed_github_app_check_run_webhook_and_assert_http_200(
        &fixture,
        build_check_run_requested_action_webhook_body("test-org/repo", "outsider"),
    )
    .await;

    assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(fixture).await;
}

#[tokio::test]
async fn check_run_approval_click_from_writer_of_other_repository_keeps_approval_pending_and_starts_no_evaluation()
 {
    let fixture =
        create_test_database_with_pending_fork_pull_request_approval_and_mock_gitea_web_server()
            .await;
    mount_collaborator_permission_endpoint_mock(
        &fixture.git_host,
        "other-org/repo",
        "maintainer",
        "write",
        0,
    )
    .await;

    post_signed_github_app_check_run_webhook_and_assert_http_200(
        &fixture,
        build_check_run_requested_action_webhook_body("other-org/repo", "maintainer"),
    )
    .await;

    assert_no_evaluation_row_and_fork_pull_request_approval_is_unapproved(fixture).await;
}
