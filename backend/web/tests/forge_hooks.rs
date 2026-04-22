/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for inbound forge webhook endpoints.
//!
//! Verifies `BaseResponse<WebhookResponse>` envelope shape across eight cases:
//! - Generic forge (Gitea): no matching project, matching project queues,
//!   invalid signature, integration not found.
//! - GitHub App: push matching project queues, ping, installation, not configured.
//!
//! Uses manual Tokio runtimes because `#[tokio::test]` expands to `::core::…`
//! which clashes with the local `core` crate name in this workspace.

use axum_test::TestServer;
use core::ci::{WebhookClient, encrypt_webhook_secret};
use core::storage::{EmailSender, NarStore};
use core::types::ServerState;
use entity::evaluation::EvaluationStatus;
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;
use test_support::cli::test_cli_with_crypt;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Write `content` to a unique file under the system temp dir and return the path.
/// The file is intentionally not cleaned up — tests are short-lived.
fn temp_secret_file(content: &str) -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::new_v4()));
    std::fs::write(&path, content).expect("write temp secret file");
    path.to_string_lossy().into_owned()
}

/// HMAC-SHA256 of `body` with `secret` as a bare hex string (Gitea format, no prefix).
fn gitea_signature(secret: &str, body: &[u8]) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// HMAC-SHA256 of `body` with `secret` as `sha256=<hex>` (GitHub format).
fn github_signature(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", gitea_signature(secret, body))
}

/// Build a `ServerState` with an optionally-overridden `crypt_secret_file` and
/// `github_app_webhook_secret_file`. When `gh_secret_path` is `Some`, the three
/// required GitHub App config fields are all set so that `github_app_config()`
/// returns `Some(…)`.
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
        // All three fields must be present for `github_app_config()` to return Some.
        cli.github_app_id = Some(1234);
        cli.github_app_private_key_file = Some("/dev/null".into());
        cli.github_app_webhook_secret_file = Some(p.clone());
    }
    let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        db,
        cli,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    })
}

// ── Fixture builders ──────────────────────────────────────────────────────────

fn fixture_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn org_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000001").unwrap()
}
fn integration_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000002").unwrap()
}
fn project_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000003").unwrap()
}
fn user_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000004").unwrap()
}
fn eval_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000005").unwrap()
}
fn commit_id() -> Uuid {
    Uuid::parse_str("a0000000-0000-0000-0000-000000000006").unwrap()
}

/// A Gitea push payload whose `clone_url` matches `https://gitea.example.com/test-org/repo`.
const GITEA_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://gitea.example.com/test-org/repo",
        "ssh_url": "git@gitea.example.com:test-org/repo.git"
    }
}"#;

/// A GitHub push payload whose `clone_url` matches `https://github.com/gh-org/repo`.
const GITHUB_PUSH_BODY: &str = r#"{
    "ref": "refs/heads/main",
    "after": "abcdef0123456789abcdef0123456789abcdef01",
    "repository": {
        "clone_url": "https://github.com/gh-org/repo",
        "ssh_url": "git@github.com:gh-org/repo.git"
    }
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
        created_by: user_id(),
        created_at: fixture_date(),
        managed: false,
        github_installation_id: None,
        github_app_enabled: false,
    }
}

fn integration_row(secret_ciphertext: &str) -> entity::integration::Model {
    entity::integration::Model {
        id: integration_id(),
        organization: org_id(),
        name: "my-hook".into(),
        display_name: "my-hook".into(),
        kind: 0, // Inbound
        forge_type: 0, // Gitea
        secret: Some(secret_ciphertext.to_string()),
        endpoint_url: None,
        access_token: None,
        created_by: user_id(),
        created_at: fixture_date(),
    }
}

fn project_row(repo_url: &str) -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: repo_url.to_string(),
        evaluation_wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: fixture_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: fixture_date(),
        managed: false,
        keep_evaluations: 10,
    }
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

// ── Test 1: Generic forge — no matching project (Gitea) ───────────────────────

#[test]
fn forge_webhook_no_matching_project() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_no_matching_project_inner().await });
}

async fn forge_webhook_no_matching_project_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!"); // 32 bytes for AES-256
    let ciphertext =
        encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt failed");

    // Mock chain:
    // 1. SELECT org by name → org row
    // 2. SELECT integration → integration row
    // 3. SELECT all active projects → empty list (no matching project)
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([Vec::<entity::project::Model>::new()])
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
        .add_header("X-Gitea-Signature", &sig)
        .bytes(body.into())
        .await;

    response.assert_status_ok();
    let json: Value = response.json();
    assert_eq!(json["error"], false, "expected error=false");
    let msg = &json["message"];
    assert_eq!(msg["event"], "push", "expected event=push");
    assert_eq!(
        msg["projects_scanned"], 0,
        "expected 0 projects_scanned"
    );
    assert!(
        msg["queued"].as_array().unwrap().is_empty(),
        "expected empty queued"
    );
    assert!(
        msg["skipped"].as_array().unwrap().is_empty(),
        "expected empty skipped"
    );
}

// ── Test 2: Generic forge — matching project queues evaluation ─────────────────

#[test]
fn forge_webhook_matching_project_queues() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_matching_project_queues_inner().await });
}

async fn forge_webhook_matching_project_queues_inner() {
    let plaintext_secret = "test-secret-plaintext";
    let crypt_path = temp_secret_file("this-is-a-32-byte-crypt-key!!!!"); // 32 bytes
    let ciphertext =
        encrypt_webhook_secret(&crypt_path, plaintext_secret).expect("encrypt failed");

    // Mock chain:
    // 1. SELECT org by name → org row
    // 2. SELECT integration → integration row
    // 3. SELECT all active projects → [matching project]
    // 4. SELECT org by id (per-project lookup) → org row
    // 5. SELECT in-progress eval → empty (trigger_evaluation step 1)
    // 6. INSERT commit → commit row
    // 7. INSERT evaluation → eval row
    // 8. SELECT project for update
    // 9. UPDATE project (exec)
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([vec![integration_row(&ciphertext)]])
        .append_query_results([vec![project_row(
            "https://gitea.example.com/test-org/repo",
        )]])
        .append_query_results([vec![org_row("test-org")]]) // per-project org lookup
        .append_query_results([Vec::<entity::evaluation::Model>::new()]) // no in-progress eval
        .append_query_results([vec![commit_row()]]) // INSERT commit
        .append_query_results([vec![eval_row(EvaluationStatus::Queued)]]) // INSERT eval
        .append_query_results([vec![project_row("https://gitea.example.com/test-org/repo")]]) // SELECT for update
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]) // UPDATE project
        .into_connection();

    let state = make_state(db, Some(crypt_path), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let body = GITEA_PUSH_BODY.as_bytes();
    let sig = gitea_signature(plaintext_secret, body);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/my-hook")
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
    assert_eq!(queued.len(), 1, "expected one queued evaluation");
    assert_eq!(
        queued[0]["project_name"], "test-project",
        "unexpected project name"
    );
    assert_eq!(
        queued[0]["organization"], "test-org",
        "unexpected organization"
    );
    assert!(
        msg["skipped"].as_array().unwrap().is_empty(),
        "expected empty skipped"
    );
}

// ── Test 3: Generic forge — invalid signature → 401 ───────────────────────────

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

    // Mock chain: org → integration (handler reads both before signature check).
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
        .add_header("X-Gitea-Signature", &wrong_sig)
        .bytes(body.into())
        .await;

    response.assert_status_unauthorized();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "invalid webhook signature");
}

// ── Test 4: Generic forge — integration not found → 404 ───────────────────────

#[test]
fn forge_webhook_integration_not_found() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { forge_webhook_integration_not_found_inner().await });
}

async fn forge_webhook_integration_not_found_inner() {
    // Mock chain: org found, integration not found.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![org_row("test-org")]])
        .append_query_results([Vec::<entity::integration::Model>::new()])
        .into_connection();

    // crypt_path doesn't matter here — we never reach decryption
    let state = make_state(db, Some(temp_secret_file("any-32-byte-secret-here!!!!!!!!")), None);
    let router = create_router(state);
    let server = TestServer::new(router);

    let response = server
        .post("/api/v1/hooks/gitea/test-org/missing-hook")
        .add_header("X-Gitea-Signature", "doesnotmatter")
        .bytes(GITEA_PUSH_BODY.as_bytes().into())
        .await;

    response.assert_status_not_found();
    let json: Value = response.json();
    assert_eq!(json["error"], true);
    assert_eq!(json["message"], "integration not found");
}

// ── Test 5: GitHub App — push matching project queues ─────────────────────────

#[test]
fn github_app_webhook_push_queues() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_push_queues_inner().await });
}

async fn github_app_webhook_push_queues_inner() {
    let gh_secret = "github-webhook-secret";
    let gh_secret_path = temp_secret_file(gh_secret);

    // Mock chain for GitHub App push:
    // 1. SELECT all active projects → [matching project]
    // 2. SELECT org by id (per-project lookup) → org row
    // 3. SELECT in-progress eval → empty
    // 4. INSERT commit → commit row
    // 5. INSERT evaluation → eval row
    // 6. SELECT project for update
    // 7. UPDATE project (exec)
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![project_row(
            "https://github.com/gh-org/repo",
        )]])
        .append_query_results([vec![org_row("gh-org")]]) // per-project org lookup
        .append_query_results([Vec::<entity::evaluation::Model>::new()])
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_row(EvaluationStatus::Queued)]])
        .append_query_results([vec![project_row("https://github.com/gh-org/repo")]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
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
    assert_eq!(json["error"], false);
    let msg = &json["message"];
    assert_eq!(msg["event"], "push");
    assert_eq!(msg["projects_scanned"], 1);

    let queued = msg["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "expected one queued evaluation");
    assert_eq!(queued[0]["project_name"], "test-project");
    assert_eq!(queued[0]["organization"], "gh-org");
    assert!(msg["skipped"].as_array().unwrap().is_empty());
}

// ── Test 6: GitHub App — ping ─────────────────────────────────────────────────

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

    // Ping → no DB queries needed
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

// ── Test 7: GitHub App — installation (org not found, just warns) ─────────────

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

    // Mock chain: store_installation_id calls SELECT org by name → None (not found).
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

// ── Test 8: GitHub App — not configured → 503 ────────────────────────────────

#[test]
fn github_app_webhook_not_configured() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { github_app_webhook_not_configured_inner().await });
}

async fn github_app_webhook_not_configured_inner() {
    // Default test_cli has github_app_webhook_secret_file = None → github_app_config() = None.
    // No DB queries expected.
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    // make_state with gh_secret_path=None → uses test_cli() → no GitHub App config.
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
