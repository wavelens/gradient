/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for inbound forge webhook endpoints.
//!
//! Tests verify `BaseResponse<WebhookResponse>` across these scenarios:
//! - Generic forge (Gitea): no matching trigger, push fires trigger, invalid
//!   signature, integration not found, non-matching branch glob is skipped,
//!   PR event fires, PR action mismatch is skipped, release event fires.
//! - GitHub App: push fires, ping, installation, not configured.
//!
//! Uses manual Tokio runtimes because `#[tokio::test]` expands to
//! `::gradient_core::…` which clashes with the local `core` crate name.

use axum_test::TestServer;
use entity::evaluation::EvaluationStatus;
use gradient_core::ci::actions::encrypt_secret_with_file as encrypt_webhook_secret;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::ids::*;
use gradient_core::types::triggers::TriggerConfig;
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;
use test_support::cli::test_cli_with_crypt;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

// ── Helpers ───────────────────────────────────────────────────────────────────

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
        cli.github_app.github_app_id = Some(1234);
        cli.github_app.github_app_private_key_file = Some("/dev/null".into());
        cli.github_app.github_app_webhook_secret_file = Some(p.clone());
    }
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: std::sync::Arc::new(
            gradient_core::types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
        ),
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
    })
}

// ── Fixture builders ──────────────────────────────────────────────────────────

fn fixture_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000001").unwrap())
}
fn integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000002").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000003").unwrap())
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
fn trigger_id() -> ProjectTriggerId {
    ProjectTriggerId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000007").unwrap())
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

const GITHUB_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://github.com/gh-org/repo",
        "ssh_url": "git@github.com:gh-org/repo.git"
    },
    "installation": { "id": 9999 }
}"#;

fn org_row(name: &str) -> entity::organization::Model {
    entity::organization::Model {
        id: org_id(),
        name: name.to_string(),
        display_name: "Test Org".into(),
        description: String::new(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        hide_build_requests: false,
        created_by: user_id(),
        created_at: fixture_date(),
        managed: false,
        github_installation_id: None,
    }
}

fn org_row_with_installation(name: &str, installation_id: i64) -> entity::organization::Model {
    let mut row = org_row(name);
    row.github_installation_id = Some(installation_id);
    row
}

fn integration_row(secret_ciphertext: &str) -> entity::integration::Model {
    entity::integration::Model {
        id: integration_id(),
        organization: org_id(),
        name: "my-hook".into(),
        display_name: "my-hook".into(),
        kind: 0,       // Inbound
        forge_type: 0, // Gitea
        secret: Some(secret_ciphertext.to_string()),
        endpoint_url: None,
        access_token: None,
        created_by: user_id(),
        created_at: fixture_date(),
    }
}

fn github_integration_row() -> entity::integration::Model {
    entity::integration::Model {
        id: integration_id(),
        organization: org_id(),
        name: "github-app".into(),
        display_name: "GitHub App".into(),
        kind: 0,       // Inbound
        forge_type: 3, // GitHub
        secret: None,
        endpoint_url: None,
        access_token: None,
        created_by: user_id(),
        created_at: fixture_date(),
    }
}

fn project_row() -> entity::project::Model {
    project_row_with(
        project_id(),
        org_id(),
        "test-project",
        "https://gitea.example.com/test-org/repo",
    )
}

fn project_row_with(
    id: ProjectId,
    organization: OrganizationId,
    name: &str,
    repository: &str,
) -> entity::project::Model {
    entity::project::Model {
        id,
        organization,
        name: name.into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: repository.into(),
        wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: fixture_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: fixture_date(),
        managed: false,
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
    }
}

fn github_project_row() -> entity::project::Model {
    project_row_with(
        project_id(),
        org_id(),
        "test-project",
        "https://github.com/gh-org/repo",
    )
}

fn eval_row(status: EvaluationStatus) -> entity::evaluation::Model {
    entity::evaluation::Model {
        id: eval_id(),
        project: Some(project_id()),
        repository: "https://gitea.example.com/test-org/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status,
        previous: None,
        next: None,
        created_at: fixture_date(),
        updated_at: fixture_date(),
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
    }
}

fn commit_row() -> entity::commit::Model {
    entity::commit::Model {
        id: commit_id(),
        message: String::new(),
        hash: vec![0u8; 20],
        author: None,
        author_name: String::new(),
    }
}

fn cache_row() -> entity::cache::Model {
    entity::cache::Model {
        id: CacheId::now_v7(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 10,
        local_priority: None,
        public_key: String::new(),
        private_key: String::new(),
        public: false,
        created_by: UserId::nil(),
        created_at: fixture_date(),
        managed: false,
    }
}

fn org_cache_row() -> entity::organization_cache::Model {
    entity::organization_cache::Model {
        id: OrganizationCacheId::now_v7(),
        organization: org_id(),
        cache: CacheId::now_v7(),
        mode: entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    }
}

fn worker_registration_row() -> entity::worker_registration::Model {
    entity::worker_registration::Model {
        id: gradient_core::types::ids::WorkerRegistrationId::now_v7(),
        peer_id: org_id(),
        worker_id: "00000000-0000-4000-8000-000000000001".into(),
        token_hash: String::new(),
        managed: false,
        url: None,
        active: true,
        enable_fetch: true,
        enable_eval: true,
        enable_build: true,
        display_name: String::new(),
        created_by: Some(gradient_core::types::ids::UserId::nil()),
        created_at: fixture_date(),
    }
}

/// Build a `project_trigger` row with the given config.
fn trigger_row(cfg: TriggerConfig) -> entity::project_trigger::Model {
    entity::project_trigger::Model {
        id: trigger_id(),
        project: project_id(),
        trigger_type: i16::from(cfg.trigger_type()),
        config: cfg.to_db_json(),
        active: true,
        last_fired_at: None,
        created_at: fixture_date(),
        updated_at: fixture_date(),
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
        // Existing fan-out tests don't depend on the approval gate. Keep
        // the legacy "run anything that matches" behaviour explicit here.
        require_approval: false,
    }
}

/// Mock DB chain for a successful `apply_trigger` call with no prior evaluation
/// (skips same-commit dedup) and no in-flight evaluation. Includes the
/// `org_has_writable_cache` lookup that runs after the eval is created,
/// the `org_has_eval_capable_worker_registration` lookup that follows it,
/// the `touch_trigger_last_fired` update on the trigger row, and the
/// `dispatch_evaluation_created` lookup of project actions.
fn apply_trigger_db_chain(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<entity::evaluation::Model>::new()]) // in-flight check
        .append_query_results([Vec::<entity::evaluation::Model>::new()]) // trigger_evaluation: in-progress check
        .append_query_results([vec![commit_row()]]) // INSERT commit
        .append_query_results([vec![eval_row(EvaluationStatus::Queued)]]) // INSERT eval
        .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()]) // snapshot flake input overrides (none)
        .append_query_results([vec![project_row()]]) // SELECT project for update
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]) // UPDATE project
        .append_query_results([vec![org_cache_row()]]) // org_has_writable_cache: subscription rows
        .append_query_results([vec![cache_row()]]) // org_has_writable_cache: active cache rows
        .append_query_results([vec![worker_registration_row()]]) // org_has_eval_capable_worker_registration
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]]) // touch_trigger_last_fired: SELECT for UPDATE
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]) // touch_trigger_last_fired: UPDATE
        .append_query_results([Vec::<entity::project_action::Model>::new()]) // dispatch_evaluation_created: project_action lookup
}

// ── Test 1: Generic forge - no matching trigger (Gitea) ───────────────────────

#[test]
fn forge_webhook_no_matching_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_no_matching_trigger_inner().await });
}

async fn forge_webhook_no_matching_trigger_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!"); // 32 bytes
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    // Mock chain:
    // 1. SELECT org by name → org row
    // 2. SELECT integration → integration row
    // 3. load_active_triggers_for_integration → empty (no trigger rows)
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([Vec::<entity::project_trigger::Model>::new()])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 2: Generic forge - push fires matching trigger ───────────────────────

#[test]
fn forge_webhook_push_fires_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_push_fires_trigger_inner().await });
}

async fn forge_webhook_push_fires_trigger_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!"); // 32 bytes
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    // Mock chain:
    // 1. SELECT org by name → org row
    // 2. SELECT integration → integration row
    // 3. load_active_triggers → [reporter_push trigger matching this integration_id]
    // 4. EProject::find_by_id → project row
    // 5. EOrganization::find_by_id (org_name_for) → org row
    // 6–11. apply_trigger chain
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row("test-org")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["projects_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["project_name"], "test-project");
    assert_eq!(queued[0]["organization"], "test-org");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 3: Generic forge - invalid signature → 401 ───────────────────────────

#[test]
fn forge_webhook_invalid_signature() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_invalid_signature_inner().await });
}

async fn forge_webhook_invalid_signature_inner() {
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, "correct-secret").expect("encrypt");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let wrong_sig = gitea_signature("wrong-secret", body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &wrong_sig)
        .bytes(body.into())
        .await;

    response.assert_status_unauthorized();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "invalid webhook signature");
}

// ── Test 4: Generic forge - integration not found → 404 ───────────────────────

#[test]
fn forge_webhook_integration_not_found() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_integration_not_found_inner().await });
}

async fn forge_webhook_integration_not_found_inner() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([Vec::<entity::integration::Model>::new()])
        .into_connection();

    let state = make_state(
        db,
        Some(temp_secret_file("any-32-byte-secret-here!!!!!!!!")),
        None,
    );
    let router = create_router(state);
    let server = TestServer::new(router);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/missing-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", "doesnotmatter")
        .bytes(GITEA_PUSH_BODY.as_bytes().into())
        .await;

    response.assert_status_not_found();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "integration not found");
}

// ── Test 5: Generic forge - branch glob non-match → skipped ──────────────────

#[test]
fn forge_webhook_branch_glob_no_match_skipped() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_branch_glob_no_match_skipped_inner().await });
}

async fn forge_webhook_branch_glob_no_match_skipped_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    // Trigger only allows "release/*" branches; push is to "feature/new-thing"
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec!["release/*"]))]])
        // project_identity lookup (for skipped entry)
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row("test-org")]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BRANCH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());

    let skipped = msg["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["reason"], "filter");
}

// ── Test 6: Generic forge - PR event fires trigger ────────────────────────────

#[test]
fn forge_webhook_pr_fires_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_pr_fires_trigger_inner().await });
}

const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

async fn forge_webhook_pr_fires_trigger_inner() {
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
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec![
            "opened",
            "synchronize",
        ]))]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row("test-org")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = pr_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "pull_request")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "pull_request");
    assert_eq!(msg["projects_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["project_name"], "test-project");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 7: Generic forge - PR action mismatch → skipped ──────────────────────

#[test]
fn forge_webhook_pr_action_mismatch_skipped() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_pr_action_mismatch_skipped_inner().await });
}

async fn forge_webhook_pr_action_mismatch_skipped_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    // Trigger only fires on "opened"; we send "closed"
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
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_pr_trigger(vec!["opened"]))]])
        // project_identity lookup for skipped
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row("test-org")]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = pr_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "pull_request")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "pull_request");
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());

    let skipped = msg["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["reason"], "filter");
}

// ── Test 8: Generic forge - release event fires releases_only trigger ─────────

#[test]
fn forge_webhook_release_fires_releases_only_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_release_fires_releases_only_trigger_inner().await });
}

async fn forge_webhook_release_fires_releases_only_trigger_inner() {
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
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row("test-org")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body_bytes: Vec<u8> = release_body.into_bytes();
    let sig = gitea_signature(plaintext_secret, &body_bytes);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "release")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body_bytes.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "release");
    assert_eq!(msg["projects_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["project_name"], "test-project");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 9: Generic forge - push does NOT fire releases_only trigger ──────────

#[test]
fn forge_webhook_push_does_not_fire_releases_only_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_push_does_not_fire_releases_only_trigger_inner().await });
}

async fn forge_webhook_push_does_not_fire_releases_only_trigger_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!");
    let ciphertext = encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        // releases_only trigger returned; push handler should skip it
        .append_query_results([vec![trigger_row(reporter_push_releases_only_trigger())]])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Event", "push")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    // silently skipped (releases_only triggers are just skipped, not added to skipped list)
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 10: GitHub App - push fires trigger ───────────────────────────────────

#[test]
fn github_app_webhook_push_fires_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_push_fires_trigger_inner().await });
}

async fn github_app_webhook_push_fires_trigger_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    // Mock chain:
    // resolve_github_app_targets:
    //   1. SELECT orgs by installation_id (.all) → [org row]
    //   2. SELECT projects for org (.all) → [project row matching webhook url]
    //   3. SELECT inbound GitHub integration (.one) → integration row
    // fan_out_triggers:
    //   4. load_active_triggers → [reporter_push trigger]
    //   5. EProject::find_by_id → project row
    //   6. org_name_for → org row
    //   7+. apply_trigger chain
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row_with_installation("gh-org", 9999)]])
        .append_query_results([vec![github_project_row()]])
        .append_query_results([vec![github_integration_row()]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_query_results([vec![github_project_row()]])
        .append_query_results([vec![org_row("gh-org")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state);
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
    assert_eq!(msg["projects_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["project_name"], "test-project");
    assert_eq!(queued[0]["organization"], "gh-org");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 11: GitHub App - ping ─────────────────────────────────────────────────

#[test]
fn github_app_webhook_ping() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_ping_inner().await });
}

async fn github_app_webhook_ping_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state);
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
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 12: GitHub App - installation (org not found, just warns) ─────────────

#[test]
fn github_app_webhook_installation() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_installation_inner().await });
}

async fn github_app_webhook_installation_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<entity::organization::Model>::new()])
        .into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = serde_json::to_vec(&serde_json::json!({
        "action": "created",
        "installation": {
            "id": 9999,
            "account": { "login": "gh-org" }
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
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 13: GitHub App - not configured → 503 ────────────────────────────────

#[test]
fn github_app_webhook_not_configured() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_not_configured_inner().await });
}

async fn github_app_webhook_not_configured_inner() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let state = make_state(db, None, None);
    let router = create_router(state);
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

// ── Test 15: GitHub App - multi-org installation routes by repo URL ─────────

#[test]
fn github_app_webhook_multi_org_routes_to_matching_org() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_multi_org_routes_to_matching_org_inner().await });
}

async fn github_app_webhook_multi_org_routes_to_matching_org_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    // Two orgs share installation_id=9999. Org A's projects don't match
    // the webhook's repo URL; org B has the matching project. Only org B's
    // integration should fire.
    let org_a_id =
        OrganizationId::new(Uuid::parse_str("a0000000-0000-0000-0000-0000000000aa").unwrap());
    let mut org_a = org_row_with_installation("org-a", 9999);
    org_a.id = org_a_id;
    let org_b = org_row_with_installation("gh-org", 9999);

    let org_a_project = project_row_with(
        ProjectId::new(Uuid::parse_str("a0000000-0000-0000-0000-0000000000ab").unwrap()),
        org_a_id,
        "unrelated",
        "https://github.com/org-a/different-repo",
    );
    let org_b_project = github_project_row();

    // Mock chain:
    // resolve_github_app_targets:
    //   1. orgs.all by installation_id → [org A, org B]
    //   2. projects.all for org A → [org_a_project]   (no URL match → skipped)
    //   3. projects.all for org B → [org_b_project]   (URL matches)
    //   4. integration.one for org B → github_integration_row
    // fan_out_triggers for org B's integration:
    //   5. load_active_triggers → [trigger]
    //   6. EProject::find_by_id → org_b_project
    //   7. org_name_for → org B row
    //   8+. apply_trigger chain
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_a.clone(), org_b.clone()]])
        .append_query_results([vec![org_a_project]])
        .append_query_results([vec![org_b_project.clone()]])
        .append_query_results([vec![github_integration_row()]])
        .append_query_results([vec![trigger_row(reporter_push_trigger(vec![]))]])
        .append_query_results([vec![org_b_project.clone()]])
        .append_query_results([vec![org_row("gh-org")]]);
    let db = apply_trigger_db_chain(db).into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state);
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
    assert_eq!(msg["projects_scanned"], 1);
    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["organization"], "gh-org");
}

// ── Test 16: GitHub App - no project matches webhook repo URL ───────────────

#[test]
fn github_app_webhook_no_matching_repo_returns_zero() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_no_matching_repo_returns_zero_inner().await });
}

async fn github_app_webhook_no_matching_repo_returns_zero_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    // Org has the installation but no project with a matching repo URL.
    let unrelated = project_row_with(
        project_id(),
        org_id(),
        "unrelated",
        "https://github.com/somewhere/else",
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row_with_installation("gh-org", 9999)]])
        .append_query_results([vec![unrelated]])
        .into_connection();

    let state = make_state(db, None, Some(gh_secret_path));
    let router = create_router(state);
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
    assert_eq!(msg["projects_scanned"], 0);
    assert!(msg["queued"].as_array().unwrap().is_empty());
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}
