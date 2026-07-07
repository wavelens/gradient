/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::log_storage::{InMemoryLogStorage, NoopLogStorage};
use futures::TryStreamExt as _;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_storage::{EmailSender, NarStore};
use gradient_types::ids::*;
use gradient_types::{RuntimeConfig, SecretString};
use harmonia_file_nar::NarByteStream;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use uuid::Uuid;

pub const FIXTURE_CACHE_NAME: &str = "test-cache";
pub const FIXTURE_PATH_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const FIXTURE_DRV_FILENAME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";

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
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
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
        hash: FIXTURE_PATH_HASH.into(),
        package: "hello".into(),
        nar_size: Some(67890),
        is_cached: true,
        cached_path: Some(cached_path_id()),
        created_at: test_date(),
        ..Default::default()
    };

    let cached_path_row = gradient_entity::cached_path::Model {
        id: cached_path_id(),
        hash: FIXTURE_PATH_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_PATH_HASH)),
        created_at: test_date(),
        ..Default::default()
    };

    let cached_path_sig_row = gradient_entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        signature: Some(vec![0x42; 64]),
        created_at: test_date(),
        ..Default::default()
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![drv_output_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![cached_path_sig_row]])
        // references_for_hash (cached_path_reference): this fixture has none.
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Build a `ServerState` with a single public, active cache named [`FIXTURE_CACHE_NAME`].
/// No `cached_path` or `derivation_output` rows are seeded - suitable for endpoint-level
/// tests that don't exercise store-path resolution (e.g. `nix-cache-info`).
pub async fn public_cache_state() -> Arc<ServerState> {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Build a `ServerState` with a public cache and a synthetic NAR stored under
/// [`FIXTURE_PATH_HASH`]. The NAR contains `bin/hello = "hi"` (2 bytes).
///
/// Mock query order:
///   0. ECache::find (by name)
///   1. ECachedPath::find (file_hash lookup - returns empty so hash falls
///      back to FIXTURE_PATH_HASH directly)
pub async fn public_cache_with_nar() -> Arc<ServerState> {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
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

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    });

    let compressed = synthetic_nar_zst().await;
    state
        .nar_storage
        .put(FIXTURE_PATH_HASH, compressed)
        .await
        .expect("put NAR into test storage");

    state
}

fn anchor_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000008").unwrap())
}

fn attempt_id() -> BuildAttemptId {
    BuildAttemptId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000011").unwrap())
}

fn cache_derivation_id() -> CacheDerivationId {
    CacheDerivationId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000010").unwrap())
}

fn cache_row() -> gradient_entity::cache::Model {
    cache_row_with_visibility(true)
}

fn cache_row_with_visibility(public: bool) -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    }
}

fn derivation_row() -> gradient_entity::derivation::Model {
    gradient_entity::derivation::Model {
        id: deriv_id(),
        hash: FIXTURE_PATH_HASH.into(),
        name: "hello".into(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn cache_derivation_row() -> gradient_entity::cache_derivation::Model {
    gradient_entity::cache_derivation::Model {
        id: cache_derivation_id(),
        cache: cache_id(),
        derivation: deriv_id(),
        cached_at: test_date(),
        ..Default::default()
    }
}

fn anchor_row(
    status: gradient_entity::build::BuildStatus,
) -> gradient_entity::derivation_build::Model {
    gradient_entity::derivation_build::Model {
        id: anchor_id(),
        derivation: deriv_id(),
        status,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn attempt_row() -> gradient_entity::build_attempt::Model {
    gradient_entity::build_attempt::Model {
        id: attempt_id(),
        derivation_build: anchor_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn make_state(
    db: sea_orm::DatabaseConnection,
    log_storage: Arc<dyn gradient_storage::LogStorage>,
) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Public cache + derivation linked via `cache_derivation` + completed build +
/// log storage seeded. Returns `(state, expected_log_body)`.
pub async fn cache_with_completed_build_in_cache() -> (Arc<ServerState>, String) {
    let log_body = "build output line 1\nbuild output line 2\n".to_string();

    let log_storage = Arc::new(InMemoryLogStorage::new());
    log_storage.seed(attempt_id(), log_body.clone());

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([vec![cache_derivation_row()]])
        .append_query_results([vec![anchor_row(
            gradient_entity::build::BuildStatus::Completed,
        )]])
        .append_query_results([vec![attempt_row()]])
        .into_connection();

    (make_state(db, log_storage), log_body)
}

/// Public cache + derivation with no `cache_derivation` link - `/log` must 404.
pub async fn cache_with_completed_build_not_in_cache() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([Vec::<gradient_entity::cache_derivation::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + derivation linked via `cache_derivation` but only a failed
/// build - `/log` must 404.
pub async fn cache_with_failed_build_only() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([vec![cache_derivation_row()]])
        .append_query_results([Vec::<gradient_entity::derivation_build::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

/// Private cache with no cached paths - suitable for auth-required tests on
/// `nix-cache-info` and `gradient-cache-info`. `CacheContext::load` returns
/// 401 Unauthorized for unauthenticated requests to private caches.
pub async fn private_cache_state() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    })
}

/// Public cache where no derivation row matches the requested `.drv` filename.
pub async fn cache_with_unknown_derivation() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::derivation::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + a completed anchor whose latest attempt's log is seeded; the
/// endpoint serves that attempt via `latest_attempt_id`. Returns
/// `(state, expected_log)`.
pub async fn cache_with_two_completed_builds() -> (Arc<ServerState>, String) {
    let newer_log = "newer build log\n".to_string();
    let log_storage = Arc::new(InMemoryLogStorage::new());
    log_storage.seed(attempt_id(), newer_log.clone());

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([vec![cache_derivation_row()]])
        .append_query_results([vec![anchor_row(
            gradient_entity::build::BuildStatus::Completed,
        )]])
        .append_query_results([vec![attempt_row()]])
        .into_connection();

    (make_state(db, log_storage), newer_log)
}

/// Private cache with a synthetic NAR - for auth-required tests on `/ls` and `/serve`.
pub async fn private_cache_with_nar() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
    });

    let compressed = synthetic_nar_zst().await;
    state
        .nar_storage
        .put(FIXTURE_PATH_HASH, compressed)
        .await
        .expect("put NAR into test storage");

    state
}

fn cached_path_row_fixture() -> gradient_entity::cached_path::Model {
    gradient_entity::cached_path::Model {
        id: cached_path_id(),
        hash: FIXTURE_PATH_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_PATH_HASH)),
        created_at: test_date(),
        ..Default::default()
    }
}

fn cached_path_sig_row_fixture() -> gradient_entity::cached_path_signature::Model {
    gradient_entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        signature: Some(vec![0x42; 64]),
        created_at: test_date(),
        last_fetched_at: Some(test_date()),
        fetch_count: 3,
    }
}

/// Public cache with no signatures - list/stats/available return empty results.
///
/// Query order for `/nars` list endpoint:
///   0. `ECache::find` (cache resolution)
///   1. raw COUNT - single row with `total = 0`
///   2. raw SELECT - empty rows
pub async fn public_cache_empty_nars() -> Arc<ServerState> {
    use sea_orm::Value;
    use std::collections::BTreeMap;

    let mut count_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    count_row.insert("total", Value::BigInt(Some(0)));

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![count_row]])
        .append_query_results([Vec::<BTreeMap<&'static str, Value>>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Private cache + no NAR rows - list/show/stats/available reject anon callers.
pub async fn private_cache_for_nars() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + one cached_path with a matching signature - show returns full detail.
pub async fn public_cache_with_one_nar() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([vec![cached_path_sig_row_fixture()]])
        // references_for_hash (cached_path_reference): this fixture has none.
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + one signed cached_path returned by the list endpoint's raw
/// JOIN query. Mock query order:
///   0. `ECache::find` (cache resolution)
///   1. raw COUNT - single row with `total = 1`
///   2. raw SELECT - one row with the cached_path + signature columns
pub async fn public_cache_list_one_signed_nar() -> Arc<ServerState> {
    use sea_orm::Value;
    use std::collections::BTreeMap;

    let mut count_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    count_row.insert("total", Value::BigInt(Some(1)));

    let mut nar_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    nar_row.insert(
        "hash",
        Value::String(Some(Box::new(FIXTURE_PATH_HASH.into()))),
    );
    nar_row.insert(
        "store_path",
        Value::String(Some(Box::new(format!(
            "/nix/store/{}-hello",
            FIXTURE_PATH_HASH
        )))),
    );
    nar_row.insert("package", Value::String(Some(Box::new("hello".into()))));
    nar_row.insert("nar_size", Value::BigInt(Some(67890)));
    nar_row.insert("file_size", Value::BigInt(Some(12345)));
    nar_row.insert(
        "created_at",
        Value::ChronoDateTime(Some(Box::new(test_date()))),
    );
    nar_row.insert(
        "last_fetched_at",
        Value::ChronoDateTime(Some(Box::new(test_date()))),
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![count_row]])
        .append_query_results([vec![nar_row]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + cached_path exists but no signature row for this cache - show 404s.
pub async fn public_cache_with_path_no_signature() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([Vec::<gradient_entity::cached_path_signature::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + path + matching signature - `available` returns true.
pub async fn public_cache_available_true() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([vec![cached_path_sig_row_fixture()]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + no cached_path row at all - `available` returns false.
pub async fn public_cache_available_false() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Public cache + a raw aggregate row matching the stats handler's SQL.
pub async fn public_cache_stats_row() -> Arc<ServerState> {
    use sea_orm::Value;
    use std::collections::BTreeMap;

    let mut row: BTreeMap<&'static str, Value> = BTreeMap::new();
    row.insert("total_nars", Value::BigInt(Some(2)));
    row.insert("total_nar_size", Value::BigInt(Some(135780)));
    row.insert("total_file_size", Value::BigInt(Some(24690)));
    row.insert(
        "last_uploaded_at",
        Value::ChronoDateTime(Some(Box::new(test_date()))),
    );
    row.insert(
        "oldest_fetched_at",
        Value::ChronoDateTime(Some(Box::new(test_date()))),
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![row]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

/// Private cache + completed build in cache - for auth-required tests on `/log`.
pub async fn private_cache_with_completed_build_in_cache() -> Arc<ServerState> {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

async fn synthetic_nar_zst() -> Vec<u8> {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin/");
    std::fs::write(bin.join("hello"), b"hi").expect("write hello");

    let exec_path = bin.join("exec");
    std::fs::write(&exec_path, b"ex").expect("write exec");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&exec_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod exec");

    std::os::unix::fs::symlink("hello", bin.join("link")).expect("create symlink");

    let chunks: Vec<bytes::Bytes> = NarByteStream::new(tmp.path().to_path_buf())
        .try_collect()
        .await
        .expect("dump NAR");
    let nar_bytes: Vec<u8> = chunks.into_iter().flatten().collect();

    zstd::encode_all(std::io::Cursor::new(nar_bytes), 1).expect("zstd compress")
}
