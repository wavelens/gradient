/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration test: `GET /cache/{cache}/nar/{path}` streams the stored NAR
//! blob straight from `nar_storage` without buffering the whole object in the
//! server heap. The response must be byte-identical to the stored blob and
//! carry an accurate `Content-Length` (the streamed body has no implicit
//! length, so the handler sets it from the storage object size).

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use gradient_types::ids::*;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

/// 32-char nix-base32 store hash the blob is written under.
const STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// 52-char nix-base32 file hash carried in the `.nar.zst` URL slug.
const FILE_HASH_NIX32: &str = "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000001").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000002").unwrap())
}
fn cached_path_id() -> CachedPathId {
    CachedPathId::new(Uuid::parse_str("30000000-0000-0000-0000-000000000003").unwrap())
}
fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

fn cache_row() -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        display_name: "Test Cache".into(),
        active: true,
        priority: 40,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn cached_path_row() -> gradient_entity::cached_path::Model {
    gradient_entity::cached_path::Model {
        id: cached_path_id(),
        hash: STORE_HASH.into(),
        package: "hello".into(),
        file_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
        created_at: test_date(),
        ..Default::default()
    }
}

/// A blob large enough to span several storage stream chunks, so the test
/// exercises reassembly rather than a single-chunk read.
fn blob() -> Vec<u8> {
    (0..256 * 1024).map(|i| (i * 31 + 7) as u8).collect()
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

#[test]
fn nar_serve_streams_stored_blob_byte_for_byte() {
    run(async {
        let cli = test_cli();

        // Query order: CacheContext::load (ECache by name) → fetch_nar_stream's
        // single resolve_effective_hash_db (ECachedPath by file_hash).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![cache_row()]])
            .append_query_results([vec![cached_path_row()]])
            .into_connection();

        let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
        let data = blob();
        nar_storage
            .put(STORE_HASH, data.clone())
            .await
            .expect("seed NAR blob");

        let state = Arc::new(ServerState {
            web_db: WebDb::new(db),
            cache_db: gradient_db::CacheDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config: Arc::new(gradient_types::RuntimeConfig::from_cli(&cli).expect("valid config")),
            log_storage: Arc::new(NoopLogStorage),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_util::http::build_client().expect("http client"),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: Arc::new(std::collections::HashMap::new()),
            scim_group_roles: Arc::new(Default::default()),
            board_events: tokio::sync::broadcast::channel(256).0,
            forge: gradient_forge::ForgeRegistry::with_builtin(),
            upstream_query: Arc::new(tokio::sync::Semaphore::new(32)),
            reactor: Arc::new(gradient_db::NoReactor),
        });

        let peer: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let router = gradient_web::create_router(state)
            .expect("router")
            .layer(MockConnectInfo(peer));
        let server = TestServer::new(router);

        let resp = server
            .get(&format!("/cache/test-cache/nar/{FILE_HASH_NIX32}.nar.zst"))
            .await;

        resp.assert_status_ok();
        assert_eq!(
            resp.header("content-type").to_str().unwrap(),
            "application/x-nix-nar",
        );
        assert_eq!(
            resp.header("content-length").to_str().unwrap(),
            data.len().to_string(),
            "streamed NAR must carry an explicit Content-Length equal to the object size",
        );
        assert_eq!(
            resp.as_bytes().as_ref(),
            data.as_slice(),
            "served body must be byte-identical to the stored blob",
        );
    });
}
