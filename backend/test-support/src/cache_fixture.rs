/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::fakes::webhooks::RecordingWebhookClient;
use crate::log_storage::NoopLogStorage;
use futures::TryStreamExt as _;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::ids::*;
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb};
use harmonia_file_nar::NarByteStream;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use uuid::Uuid;

pub const FIXTURE_CACHE_NAME: &str = "test-cache";
pub const FIXTURE_PATH_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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

/// Build a `ServerState` pre-populated with all DB rows required for `get_nar_by_hash`
/// to resolve [`FIXTURE_PATH_HASH`] against [`FIXTURE_CACHE_NAME`].
///
/// Query order matches the one inside `get_nar_by_hash`:
///   0. ECache::find (by name)
///   1. EDerivationOutput::find (by hash)
///   2. EDerivation::find_by_id
///   3. EOrganizationCache::find (subscription check)
///   4. ECachedPath::find (by hash)
///   5. ECachedPathSignature::find
pub async fn public_cache_with_narinfo() -> Arc<ServerState> {
    let cache_row = entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        managed: false,
    };

    let drv_output_row = entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: deriv_id(),
        name: "out".into(),
        output: format!("/nix/store/{}-hello", FIXTURE_PATH_HASH),
        hash: FIXTURE_PATH_HASH.into(),
        package: "hello".into(),
        ca: None,
        nar_size: Some(67890),
        is_cached: true,
        cached_path: Some(cached_path_id()),
        created_at: test_date(),
    };

    let deriv_row = entity::derivation::Model {
        id: deriv_id(),
        organization: org_id(),
        derivation_path: format!("/nix/store/{}-hello.drv", FIXTURE_PATH_HASH),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
    };

    let org_cache_row = entity::organization_cache::Model {
        id: org_cache_id(),
        organization: org_id(),
        cache: cache_id(),
        mode: entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    };

    let cached_path_row = entity::cached_path::Model {
        id: cached_path_id(),
        store_path: format!("/nix/store/{}-hello", FIXTURE_PATH_HASH),
        hash: FIXTURE_PATH_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        references: Some(String::new()),
        ca: None,
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_PATH_HASH)),
        created_at: test_date(),
    };

    let cached_path_sig_row = entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        signature: Some(vec![0x42; 64]),
        created_at: test_date(),
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![drv_output_row]])
        .append_query_results([vec![deriv_row]])
        .append_query_results([vec![org_cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![cached_path_sig_row]])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

/// Build a `ServerState` with a single public, active cache named [`FIXTURE_CACHE_NAME`].
/// No `cached_path` or `derivation_output` rows are seeded — suitable for endpoint-level
/// tests that don't exercise store-path resolution (e.g. `nix-cache-info`).
pub async fn public_cache_state() -> Arc<ServerState> {
    let cache_row = entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        managed: false,
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

/// Build a `ServerState` with a public cache and a synthetic NAR stored under
/// [`FIXTURE_PATH_HASH`]. The NAR contains `bin/hello = "hi"` (2 bytes).
///
/// Mock query order:
///   0. ECache::find (by name)
///   1. ECachedPath::find (file_hash lookup — returns empty so hash falls
///      back to FIXTURE_PATH_HASH directly)
pub async fn public_cache_with_nar() -> Arc<ServerState> {
    let cache_row = entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        managed: false,
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([Vec::<entity::cached_path::Model>::new()])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    });

    let compressed = synthetic_nar_zst().await;
    state
        .nar_storage
        .put(FIXTURE_PATH_HASH, compressed)
        .await
        .expect("put NAR into test storage");

    state
}

async fn synthetic_nar_zst() -> Vec<u8> {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin/");
    std::fs::write(bin.join("hello"), b"hi").expect("write hello");

    let chunks: Vec<bytes::Bytes> = NarByteStream::new(tmp.path().to_path_buf())
        .try_collect()
        .await
        .expect("dump NAR");
    let nar_bytes: Vec<u8> = chunks.into_iter().flatten().collect();

    zstd::encode_all(std::io::Cursor::new(nar_bytes), 1).expect("zstd compress")
}
