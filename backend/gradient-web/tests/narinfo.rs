/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration test: `.narinfo` handler serves metadata from DB rows only.

use axum_test::TestServer;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::ids::*;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use uuid::Uuid;
use gradient_web::create_router;

// ── Fixture IDs ───────────────────────────────────────────────────────────────

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap())
}
fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
}
fn deriv_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000003").unwrap())
}
fn drv_output_id() -> DerivationOutputId {
    DerivationOutputId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000004").unwrap())
}
fn cached_path_id() -> CachedPathId {
    CachedPathId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000005").unwrap())
}
fn cached_path_sig_id() -> CachedPathSignatureId {
    CachedPathSignatureId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000006").unwrap())
}
fn org_cache_id() -> OrganizationCacheId {
    OrganizationCacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000007").unwrap())
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
/// Uses a manual Tokio runtime because `#[tokio::test]` expands to `::gradient_core::…`
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

    // A public, active cache - no HTTP auth required.
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    };

    // The derivation output matching FIXTURE_HASH.
    let drv_output_row = gradient_entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: deriv_id(),
        name: "out".into(),
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        nar_size: Some(67890),
        is_cached: true,
        cached_path: Some(cached_path_id()),
        created_at: test_date(),
        ..Default::default()
    };

    // The parent derivation, owned by org_id().
    let deriv_row = gradient_entity::derivation::Model {
        id: deriv_id(),
        organization: org_id(),
        hash: FIXTURE_HASH.into(),
        name: "hello".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
        ..Default::default()
    };

    // Subscription row proving org_id() can access cache_id().
    let org_cache_row = gradient_entity::organization_cache::Model {
        id: org_cache_id(),
        organization: org_id(),
        cache: cache_id(),
        mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    };

    // The cached_path row carrying the NAR metadata written by the worker.
    let cached_path_row = gradient_entity::cached_path::Model {
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
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_HASH)),
        created_at: test_date(),
        ..Default::default()
    };

    // Signature row for this cache.
    let cached_path_sig_row = gradient_entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        // 64 raw bytes - any non-empty Ed25519-shaped buffer works.
        signature: Some(vec![0x42; 64]),
        created_at: test_date(),
        ..Default::default()
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
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
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
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
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
    assert!(
        body.contains(&format!("Deriver: /nix/store/{FIXTURE_HASH}-hello.drv")),
        "narinfo body missing Deriver field from cached_path row:\n{body}"
    );
}

/// Regression: when the `cached_path_signature` row's `signature` column is
/// `NULL` (the state the sign sweep leaves rows in for `sign_cache=false`
/// projects), the narinfo handler must return 404 - never serve an unsigned
/// narinfo. The whole `sign_cache=false` privacy guarantee depends on this.
#[test]
fn narinfo_returns_404_when_signature_null() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { narinfo_unsigned_inner().await });
}

async fn narinfo_unsigned_inner() {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    };

    let drv_output_row = gradient_entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: deriv_id(),
        name: "out".into(),
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        nar_size: Some(67890),
        is_cached: true,
        cached_path: Some(cached_path_id()),
        created_at: test_date(),
        ..Default::default()
    };

    let deriv_row = gradient_entity::derivation::Model {
        id: deriv_id(),
        organization: org_id(),
        hash: FIXTURE_HASH.into(),
        name: "hello".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
        ..Default::default()
    };

    let org_cache_row = gradient_entity::organization_cache::Model {
        id: org_cache_id(),
        organization: org_id(),
        cache: cache_id(),
        mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    };

    let cached_path_row = gradient_entity::cached_path::Model {
        id: cached_path_id(),
        store_path: format!("/nix/store/{}-hello", FIXTURE_HASH),
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        references: Some(String::new()),
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_HASH)),
        created_at: test_date(),
        ..Default::default()
    };

    let unsigned_sig_row = gradient_entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        created_at: test_date(),
        ..Default::default()
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![drv_output_row]])
        .append_query_results([vec![deriv_row]])
        .append_query_results([vec![org_cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![unsigned_sig_row]])
        .into_connection();

    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
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
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
    });

    let router = create_router(state);
    let server = TestServer::new(router);

    let response = server
        .get(&format!("/cache/test-cache/{}.narinfo", FIXTURE_HASH))
        .await;

    response.assert_status_not_found();
}
