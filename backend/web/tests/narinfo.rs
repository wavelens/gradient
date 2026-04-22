/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration test: `.narinfo` handler serves metadata from DB rows only.

use axum_test::TestServer;
use core::ci::WebhookClient;
use core::storage::{EmailSender, NarStore};
use core::types::ServerState;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

// ── Fixture IDs ───────────────────────────────────────────────────────────────

fn cache_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap()
}
fn org_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap()
}
fn deriv_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000003").unwrap()
}
fn drv_output_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000004").unwrap()
}
fn cached_path_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000005").unwrap()
}
fn cached_path_sig_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000006").unwrap()
}
fn org_cache_id() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000007").unwrap()
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// 32-char nix-base32 hash used for the fixture store path.
const FIXTURE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ── Test ─────────────────────────────────────────────────────────────────────

/// Verify that the narinfo handler returns a well-formed response that includes
/// `NarHash:`, `NarSize:`, and `Sig:` fields served entirely from the DB rows.
///
/// Uses a manual Tokio runtime because `#[tokio::test]` expands to `::core::…`
/// references that clash with the local `core` crate name in the workspace.
#[test]
fn narinfo_served_from_db_without_daemon_probe() {
    // Build the runtime first so `create_router` (which spawns scheduler tasks)
    // runs inside a live Tokio context.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { narinfo_served_from_db_inner().await });
}

async fn narinfo_served_from_db_inner() {
    // ── Build mock DB rows ────────────────────────────────────────────────

    // A public, active cache — no HTTP auth required.
    let cache_row = entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: org_id(),
        created_at: test_date(),
        managed: false,
    };

    // The derivation output matching FIXTURE_HASH.
    let drv_output_row = entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: deriv_id(),
        name: "out".into(),
        output: format!("/nix/store/{}-hello", FIXTURE_HASH),
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        ca: None,
        // 64-char hex SHA-256 (all zeros for simplicity).
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        is_cached: true,
        cached_path: Some(cached_path_id()),
        created_at: test_date(),
    };

    // The parent derivation, owned by org_id().
    let deriv_row = entity::derivation::Model {
        id: deriv_id(),
        organization: org_id(),
        derivation_path: format!("/nix/store/{}-hello.drv", FIXTURE_HASH),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
    };

    // Subscription row proving org_id() can access cache_id().
    let org_cache_row = entity::organization_cache::Model {
        id: org_cache_id(),
        organization: org_id(),
        cache: cache_id(),
        mode: entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    };

    // The cached_path row carrying the NAR metadata written by the worker.
    let cached_path_row = entity::cached_path::Model {
        id: cached_path_id(),
        store_path: format!("/nix/store/{}-hello", FIXTURE_HASH),
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        // Valid nix32 SHA-256 (of the empty string, as a stable test vector).
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        references: Some(String::new()),
        ca: None,
        created_at: test_date(),
    };

    // Signature row for this cache.
    let cached_path_sig_row = entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        // base64("test-signature") — any non-empty value works.
        signature: Some("dGVzdC1zaWduYXR1cmU=".into()),
        created_at: test_date(),
    };

    // Query order driven by CacheContext::load + get_nar_by_hash:
    //   0. ECache::find (by name)              → cache_row
    //   1. EDerivationOutput::find (by hash)   → drv_output_row
    //   2. EDerivation::find_by_id             → deriv_row
    //   3. EOrganizationCache::find (sub check)→ org_cache_row
    //   4. ECachedPath::find (by hash)         → cached_path_row
    //   5. ECachedPathSignature::find          → cached_path_sig_row
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![drv_output_row]])
        .append_query_results([vec![deriv_row]])
        .append_query_results([vec![org_cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![cached_path_sig_row]])
        .into_connection();

    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        db,
        cli,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });

    let router = create_router(state);
    let server = TestServer::new(router);

    // ── Issue GET /cache/test-cache/<hash>.narinfo ────────────────────────
    let response = server
        .get(&format!("/cache/test-cache/{}.narinfo", FIXTURE_HASH))
        .await;

    // ── Assertions ────────────────────────────────────────────────────────
    response.assert_status_ok();
    let body = response.text();

    assert!(
        body.contains("NarHash:"),
        "narinfo body missing NarHash field:\n{body}"
    );
    assert!(
        body.contains("NarSize:"),
        "narinfo body missing NarSize field:\n{body}"
    );
    assert!(
        body.contains("Sig:"),
        "narinfo body missing Sig field:\n{body}"
    );
}
