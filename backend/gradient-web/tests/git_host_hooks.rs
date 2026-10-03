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
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli_with_crypt;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use gradient_types::GitHostType;
use gradient_types::ids::*;
use gradient_types::triggers::{ConcurrencyPolicy, TriggerConfig};
use gradient_web::create_router;
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

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
    let mut cli = match crypt_path {
        Some(ref p) => test_cli_with_crypt(p.clone()),
        None => test_cli(),
    };
    if let Some(ref p) = gh_secret_path {
        cli.github_app.id = Some(1234);
        cli.github_app.private_key_file = Some("/dev/null".into());
        cli.github_app.webhook_secret_file = Some(p.clone());
    }
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: std::sync::Arc::new(
            gradient_types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
        ),
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    })
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
fn integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000002").unwrap())
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

fn github_installation_id() -> gradient_entity::ids::GithubInstallationId {
    gradient_entity::ids::GithubInstallationId::new(
        Uuid::parse_str("a0000000-0000-0000-0000-000000000008").unwrap(),
    )
}

fn github_installation_row(
    project: gradient_types::ids::ProjectId,
    installation_id: i64,
) -> gradient_entity::github_installation::Model {
    gradient_entity::github_installation::Model {
        id: github_installation_id(),
        project,
        installation_id,
        account_login: Some("gh-project".into()),
        created_by: user_id(),
        created_at: fixture_date(),
    }
}

fn integration_row(secret_ciphertext: &str) -> gradient_entity::integration::Model {
    gradient_entity::integration::Model {
        id: integration_id(),
        project: project_id(),
        name: "my-hook".into(),
        display_name: "my-hook".into(),
        secret: Some(secret_ciphertext.to_string()),
        created_by: user_id(),
        created_at: fixture_date(),
        ..Default::default()
    }
}

fn github_integration_row() -> gradient_entity::integration::Model {
    gradient_entity::integration::Model {
        id: integration_id(),
        project: project_id(),
        name: "github-app".into(),
        display_name: "GitHub App".into(),
        git_host_type: GitHostType::GitHub,
        github_installation: Some(github_installation_id()),
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
        sign_cache: true,
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
    }
}

fn worker_registration_row() -> gradient_entity::worker_registration::Model {
    gradient_entity::worker_registration::Model {
        id: gradient_types::ids::WorkerRegistrationId::now_v7(),
        peer_id: project_id(),
        worker_id: "00000000-0000-4000-8000-000000000001".into(),
        active: true,
        enable_fetch: true,
        enable_eval: true,
        enable_build: true,
        created_by: Some(gradient_types::ids::UserId::nil()),
        created_at: fixture_date(),
        ..Default::default()
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
        integration_id: integration_id(),
        branches: branches.into_iter().map(String::from).collect(),
        tags: vec![],
        releases_only: false,
    }
}

fn reporter_push_releases_only_trigger() -> TriggerConfig {
    TriggerConfig::ReporterPush {
        integration_id: integration_id(),
        branches: vec![],
        tags: vec![],
        releases_only: true,
    }
}

fn reporter_pr_trigger(actions: Vec<&str>) -> TriggerConfig {
    TriggerConfig::ReporterPullRequest {
        integration_id: integration_id(),
        branches: vec![],
        actions: actions.into_iter().map(String::from).collect(),
        require_approval: false,
    }
}

fn apply_trigger_db_chain(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_row(EvaluationStatus::Queued)]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![project_cache_row()]])
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
        .append_query_results([vec![worker_registration_row()]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
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
async fn git_host_webhook_integration_not_found() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
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
    assert_eq!(json["message"], "integration not found");
}

#[tokio::test]
async fn git_host_webhook_branch_glob_no_match_skipped() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec!["release/*"]))]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec![
            "opened",
            "synchronize",
        ]))]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec!["opened"]))]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row("test-project")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![github_installation_row(project_id(), 9999)]])
        .append_query_results([vec![github_task_row()]])
        .append_query_results([vec![github_integration_row()]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
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

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

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

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

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
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

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
    assert_eq!(json["message"], "github app integration not configured");
}

#[tokio::test]
async fn github_app_webhook_multi_project_routes_to_matching_project() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let project_a_id =
        ProjectId::new(Uuid::parse_str("a0000000-0000-0000-0000-0000000000aa").unwrap());
    let inst_a_id = gradient_entity::ids::GithubInstallationId::new(
        Uuid::parse_str("a0000000-0000-0000-0000-0000000000a8").unwrap(),
    );
    let inst_a = gradient_entity::github_installation::Model {
        id: inst_a_id,
        project: project_a_id,
        installation_id: 9999,
        account_login: Some("project-a".into()),
        created_by: user_id(),
        created_at: fixture_date(),
    };
    let inst_b = github_installation_row(project_id(), 9999);

    let project_a_task = task_row_with(
        TaskId::new(Uuid::parse_str("a0000000-0000-0000-0000-0000000000ab").unwrap()),
        project_a_id,
        "unrelated",
        "https://github.com/org-a/different-repo",
    );
    let project_b_task = github_task_row();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![inst_a, inst_b]])
        .append_query_results([vec![project_a_task, project_b_task.clone()]])
        .append_query_results([vec![github_integration_row()]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_query_results([vec![project_b_task.clone()]])
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
    let msg = &json["message"];
    assert_eq!(msg["tasks_scanned"], 1);
    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["project"], "gh-project");
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![github_installation_row(project_id(), 9999)]])
        .append_query_results([vec![unrelated]])
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
