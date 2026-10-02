/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

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

const STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
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
        confirmed: true,
        ..Default::default()
    }
}

fn served() -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
    std::collections::BTreeMap::from([("served", sea_orm::Value::Int(Some(1)))])
}

/// The blob must span several storage stream chunks to exercise reassembly.
fn blob() -> Vec<u8> {
    (0..256 * 1024).map(|i| (i * 31 + 7) as u8).collect()
}

fn state(
    cli: &gradient_types::Cli,
    db: sea_orm::DatabaseConnection,
    nar_storage: NarStore,
) -> Arc<ServerState> {
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(gradient_types::RuntimeConfig::from_cli(cli).expect("valid config")),
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        download_progress: gradient_core::download_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        scim_group_roles: Arc::new(Default::default()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: Arc::new(tokio::sync::Semaphore::new(32)),
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

#[tokio::test]
async fn nar_serve_streams_stored_blob_byte_for_byte() {
    let cli = test_cli();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([vec![served()]])
        .into_connection();

    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    let data = blob();
    nar_storage
        .put(STORE_HASH, data.clone())
        .await
        .expect("seed NAR blob");

    let state = state(&cli, db, nar_storage);

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
}

#[tokio::test]
async fn nar_serve_answers_from_the_hot_cache_on_the_second_request() {
    let cli = test_cli();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([vec![served()]])
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([vec![served()]])
        .into_connection();

    let nar_storage = NarStore::local(&cli.server.base_dir)
        .expect("create test NarStore")
        .with_hot_cache(gradient_storage::HotNarCache::new(
            4 * 1024 * 1024,
            1024 * 1024,
        ));
    let data = blob();
    nar_storage
        .put(STORE_HASH, data.clone())
        .await
        .expect("seed NAR blob");
    let hot = nar_storage.clone();

    let state = state(&cli, db, nar_storage);

    let peer: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(peer));
    let server = TestServer::new(router);
    let url = format!("/cache/test-cache/nar/{FILE_HASH_NIX32}.nar.zst");

    let first = server.get(&url).await;
    first.assert_status_ok();
    assert_eq!(first.as_bytes().as_ref(), data.as_slice());
    assert_eq!(
        hot.hot().stats().entries,
        1,
        "the first read filled the cache"
    );

    let second = server.get(&url).await;
    second.assert_status_ok();
    assert_eq!(
        second.header("content-length").to_str().unwrap(),
        data.len().to_string()
    );
    assert_eq!(second.as_bytes().as_ref(), data.as_slice());
    assert_eq!(hot.hot().stats().hits, 1, "the second read was a hit");
}

#[tokio::test]
async fn a_nar_this_cache_holds_no_claim_on_is_not_served() {
    let cli = test_cli();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
        .into_connection();
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage
        .put(STORE_HASH, blob())
        .await
        .expect("seed NAR blob");
    let state = state(&cli, db, nar_storage);

    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));
    let resp = TestServer::new(router)
        .get(&format!("/cache/test-cache/nar/{FILE_HASH_NIX32}.nar.zst"))
        .await;

    resp.assert_status_not_found();
}
