/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::log_storage::{InMemoryLogStorage, NoopLogStorage};
use crate::state::TestState;
use futures::TryStreamExt as _;
use gradient_core::ServerState;
use gradient_fixtures::mock::mock_db;
use gradient_types::ids::*;
use harmonia_file_nar::NarByteStream;
use std::sync::Arc;
use uuid::Uuid;

pub const FIXTURE_CACHE_NAME: &str = "test-cache";
pub const FIXTURE_PATH_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const FIXTURE_DRV_FILENAME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap())
}

fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
}

fn deriv_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000003").unwrap())
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

/// The mock results are following the query order: cache by name, cached path by hash, its
/// signature, references.
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
        created_by: UserId::new(project_id().into_inner()),
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

    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![cached_path_sig_row]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();

    TestState::new().web_db(db).build()
}

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
        created_by: UserId::new(project_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    };

    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .into_connection();

    TestState::new().web_db(db).build()
}

pub async fn public_cache_with_nar() -> Arc<ServerState> {
    public_cache_storing_nar(true).await
}

pub async fn public_cache_with_foreign_nar() -> Arc<ServerState> {
    public_cache_storing_nar(false).await
}

async fn public_cache_storing_nar(served: bool) -> Arc<ServerState> {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 30,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(project_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    };

    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .append_query_results([served_gate(served)])
        .into_connection();

    let state = TestState::new().web_db(db).build();

    let compressed = synthetic_nar_zst().await;
    state
        .nar_storage
        .put(FIXTURE_PATH_HASH, compressed)
        .await
        .expect("put NAR into test storage");

    state
}

fn served_gate(served: bool) -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
    let row = std::collections::BTreeMap::from([("served", sea_orm::Value::Int(Some(1)))]);
    if served { vec![row] } else { Vec::new() }
}

fn shared_build_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000008").unwrap())
}

fn attempt_id() -> BuildAttemptId {
    BuildAttemptId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000011").unwrap())
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
        created_by: UserId::new(project_id().into_inner()),
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

fn shared_build_row(
    status: gradient_entity::build::BuildStatus,
) -> gradient_entity::derivation_build::Model {
    gradient_entity::derivation_build::Model {
        id: shared_build_id(),
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
        derivation_build: shared_build_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn make_state(
    db: sea_orm::DatabaseConnection,
    log_storage: Arc<dyn gradient_storage::LogStorage>,
) -> Arc<ServerState> {
    TestState::new().web_db(db).log_storage(log_storage).build()
}

pub async fn cache_with_completed_build_in_cache() -> (Arc<ServerState>, String) {
    let log_body = "build output line 1\nbuild output line 2\n".to_string();

    let log_storage = Arc::new(InMemoryLogStorage::new());
    log_storage.seed(attempt_id(), log_body.clone());

    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([vec![shared_build_row(
            gradient_entity::build::BuildStatus::Completed,
        )]])
        .append_query_results([vec![attempt_row()]])
        .into_connection();

    (make_state(db, log_storage), log_body)
}

pub async fn cache_with_completed_build_not_in_cache() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::derivation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_upstream::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn cache_with_failed_build_only() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([Vec::<gradient_entity::derivation_build::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_upstream::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn private_cache_state() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .into_connection();

    TestState::new().web_db(db).build()
}

pub async fn cache_with_unknown_derivation() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::derivation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_upstream::Model>::new()])
        .into_connection();

    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn cache_with_two_completed_builds() -> (Arc<ServerState>, String) {
    let newer_log = "newer build log\n".to_string();
    let log_storage = Arc::new(InMemoryLogStorage::new());
    log_storage.seed(attempt_id(), newer_log.clone());

    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![derivation_row()]])
        .append_query_results([vec![shared_build_row(
            gradient_entity::build::BuildStatus::Completed,
        )]])
        .append_query_results([vec![attempt_row()]])
        .into_connection();

    (make_state(db, log_storage), newer_log)
}

pub async fn private_cache_with_nar() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();

    let state = TestState::new().web_db(db).build();

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

pub async fn public_cache_empty_nars() -> Arc<ServerState> {
    use sea_orm::Value;
    use std::collections::BTreeMap;

    let mut count_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    count_row.insert("total", Value::BigInt(Some(0)));

    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![count_row]])
        .append_query_results([Vec::<BTreeMap<&'static str, Value>>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn private_cache_for_nars() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row_with_visibility(false)]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn public_cache_with_one_nar() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([vec![cached_path_sig_row_fixture()]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn public_cache_list_one_signed_nar() -> Arc<ServerState> {
    use sea_orm::Value;
    use std::collections::BTreeMap;

    let mut count_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    count_row.insert("total", Value::BigInt(Some(1)));

    let mut nar_row: BTreeMap<&'static str, Value> = BTreeMap::new();
    nar_row.insert("hash", Value::String(Some(FIXTURE_PATH_HASH.into())));
    nar_row.insert(
        "store_path",
        Value::String(Some(format!("/nix/store/{}-hello", FIXTURE_PATH_HASH))),
    );
    nar_row.insert("package", Value::String(Some("hello".into())));
    nar_row.insert("nar_size", Value::BigInt(Some(67890)));
    nar_row.insert("file_size", Value::BigInt(Some(12345)));
    nar_row.insert("created_at", Value::ChronoDateTime(Some(test_date())));
    nar_row.insert("last_fetched_at", Value::ChronoDateTime(Some(test_date())));

    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![count_row]])
        .append_query_results([vec![nar_row]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn public_cache_with_path_no_signature() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([Vec::<gradient_entity::cached_path_signature::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn public_cache_available_true() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row_fixture()]])
        .append_query_results([vec![cached_path_sig_row_fixture()]])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn public_cache_available_false() -> Arc<ServerState> {
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
        .into_connection();
    make_state(db, Arc::new(NoopLogStorage))
}

pub async fn private_cache_with_completed_build_in_cache() -> Arc<ServerState> {
    let db = mock_db()
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
