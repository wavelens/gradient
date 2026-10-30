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
use gradient_entity::{MappingContext, RotatedHashMapping, StorePathMapping};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::mock::mock_db;
use gradient_storage::NarStore;
use gradient_test_support::prelude::test_cli;
use gradient_test_support::state::TestState;
use gradient_types::ids::*;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
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
    (0..256 * 1024u32)
        .map(|i| (i * 31 + 7).to_le_bytes()[0])
        .collect()
}

#[tokio::test]
async fn nar_serve_streams_stored_blob_byte_for_byte() {
    let cli = test_cli();

    let db = mock_db()
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

    let state = TestState::new().web_db(db).nar_storage(nar_storage).build();

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
    let db = mock_db()
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

    let state = TestState::new().web_db(db).nar_storage(nar_storage).build();

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
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
        .into_connection();
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage
        .put(STORE_HASH, blob())
        .await
        .expect("seed NAR blob");
    let state = TestState::new().web_db(db).nar_storage(nar_storage).build();

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

#[tokio::test]
async fn a_nar_url_file_hash_is_looked_up_unmapped() {
    let cli = test_cli();
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![cached_path_row()]])
        .append_query_results([vec![served()]])
        .into_connection();
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage
        .put(STORE_HASH, blob())
        .await
        .expect("seed NAR blob");
    let state = TestState::new()
        .web_db(db.clone())
        .nar_storage(nar_storage)
        .store_path_mapping(Arc::new(RotatedHashMapping))
        .build();

    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));
    TestServer::new(router)
        .get(&format!("/cache/test-cache/nar/{FILE_HASH_NIX32}.nar.zst"))
        .await
        .assert_status_ok();

    let rotated = RotatedHashMapping
        .inbound_prefix(MappingContext::Cache(cache_id()), FILE_HASH_NIX32)
        .unwrap();
    let log = format!("{:?}", db.into_transaction_log());
    assert!(log.contains(&format!("sha256:{FILE_HASH_NIX32}")), "{log}");
    assert!(!log.contains(&rotated), "{log}");
}

fn empty_directory_nar() -> Vec<u8> {
    ["nix-archive-1", "(", "type", "directory", ")"]
        .iter()
        .flat_map(|token| {
            let mut field = (token.len() as u64).to_le_bytes().to_vec();
            field.extend_from_slice(token.as_bytes());
            field.resize(8 + token.len().next_multiple_of(8), 0);
            field
        })
        .collect()
}

async fn insert_signed_path(
    db: &sea_orm::DatabaseConnection,
    cache: CacheId,
    hash: &str,
    file_hash: &str,
) {
    let path = gradient_entity::cached_path::Model {
        id: CachedPathId::now_v7(),
        hash: hash.into(),
        package: "system-shutdown".into(),
        file_hash: Some(file_hash.into()),
        file_size: Some(79),
        nar_size: Some(96),
        nar_hash: Some(file_hash.into()),
        confirmed: true,
        created_at: test_date(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
    gradient_entity::cached_path_signature::Model {
        id: CachedPathSignatureId::now_v7(),
        cached_path: path.id,
        cache,
        signature: Some(vec![0; 64]),
        created_at: test_date(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn an_empty_directory_nar_shared_with_another_cache_is_served() {
    let cli = test_cli();
    let db = TestDatabase::create().await;
    let connection = db.connection();
    let owner = gradient_fixtures::rows::user();
    owner
        .clone()
        .into_active_model()
        .insert(&connection)
        .await
        .unwrap();
    let (other, requested) = (CacheId::now_v7(), CacheId::now_v7());
    for (id, name) in [(other, "other-cache"), (requested, "test-cache")] {
        gradient_entity::cache::Model {
            public: true,
            ..gradient_fixtures::rows::cache_with_id(id, name, owner.id)
        }
        .into_active_model()
        .insert(&connection)
        .await
        .unwrap();
    }
    let file_hash = format!("sha256:{FILE_HASH_NIX32}");
    let other_path = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    insert_signed_path(&connection, other, other_path, &file_hash).await;
    insert_signed_path(&connection, requested, STORE_HASH, &file_hash).await;

    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    for hash in [other_path, STORE_HASH] {
        nar_storage
            .put(hash, empty_directory_nar())
            .await
            .expect("seed NAR blob");
    }
    let state = TestState::new()
        .web_db(connection)
        .nar_storage(nar_storage)
        .build();
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));

    let resp = TestServer::new(router)
        .get(&format!("/cache/test-cache/nar/{FILE_HASH_NIX32}.nar.zst"))
        .await;

    resp.assert_status_ok();
    assert_eq!(resp.as_bytes().as_ref(), empty_directory_nar().as_slice());
    db.finish().await;
}
