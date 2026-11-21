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
use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
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

fn served(hash: &str) -> gradient_entity::cached_path::Model {
    gradient_entity::cached_path::Model {
        id: CachedPathId::now_v7(),
        hash: hash.into(),
        nar_object_storage_key: hash.into(),
        package: "hello".into(),
        file_hash: format!("sha256:{FILE_HASH_NIX32}"),
        created_at: test_date(),
        ..Default::default()
    }
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
        .append_query_results([vec![served(STORE_HASH)]])
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
        .get(&format!("/cache/test-cache/nar/{STORE_HASH}.nar.zst"))
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
        .append_query_results([vec![served(STORE_HASH)]])
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![served(STORE_HASH)]])
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
    let url = format!("/cache/test-cache/nar/{STORE_HASH}.nar.zst");

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
        .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
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
        .get(&format!("/cache/test-cache/nar/{STORE_HASH}.nar.zst"))
        .await;

    resp.assert_status_not_found();
}

#[tokio::test]
async fn a_nar_named_by_its_file_hash_is_not_found() {
    let state = TestState::new().web_db(mock_db().into_connection()).build();
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));
    let server = TestServer::new(router);

    for name in [
        format!("{FILE_HASH_NIX32}.nar.zst"),
        format!("{FILE_HASH_NIX32}.nar"),
        format!("{STORE_HASH}.narinfo"),
    ] {
        server
            .get(&format!("/cache/test-cache/nar/{name}"))
            .await
            .assert_status_not_found();
    }
}

#[tokio::test]
async fn a_nar_url_store_hash_is_mapped_inbound() {
    let cli = test_cli();
    let db = mock_db()
        .append_query_results([vec![cache_row()]])
        .append_query_results([vec![served(EXAMPLE_STORED_HASH)]])
        .into_connection();
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage
        .put(EXAMPLE_STORED_HASH, blob())
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
    let resp = TestServer::new(router)
        .get(&format!(
            "/cache/test-cache/nar/{EXAMPLE_ORIGINAL_HASH}.nar.zst"
        ))
        .await;

    resp.assert_status_ok();
    assert_eq!(resp.as_bytes().as_ref(), blob().as_slice());
    let log = format!("{:?}", db.into_transaction_log());
    assert!(log.contains(EXAMPLE_STORED_HASH), "{log}");
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
    insert_signed_path_with_object_storage_key(db, cache, hash, hash, file_hash).await;
}

async fn insert_signed_path_with_object_storage_key(
    db: &sea_orm::DatabaseConnection,
    cache: CacheId,
    hash: &str,
    object_key: &str,
    file_hash: &str,
) {
    let path = gradient_entity::cached_path::Model {
        id: CachedPathId::now_v7(),
        hash: hash.into(),
        nar_object_storage_key: object_key.into(),
        package: "system-shutdown".into(),
        file_hash: file_hash.into(),
        file_size: Some(79),
        nar_size: 96,
        nar_hash: file_hash.into(),
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
    gradient_entity::store_path_content_producer::Model {
        id: StorePathContentProducerId::now_v7(),
        cached_path: path.id,
        kind: gradient_entity::store_path_content_producer::StoredContentVariantProducerKind::UploadedByUserOrApiKey,
        cache: Some(cache),
        created_at: test_date(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn paths_sharing_identical_bytes_are_served_by_their_own_store_hash() {
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
    let sibling_path = "cccccccccccccccccccccccccccccccc";
    let other_cache_path = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    insert_signed_path(&connection, requested, STORE_HASH, &file_hash).await;
    insert_signed_path(&connection, requested, sibling_path, &file_hash).await;
    insert_signed_path(&connection, other, other_cache_path, &file_hash).await;

    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    for hash in [STORE_HASH, sibling_path, other_cache_path] {
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

    let server = TestServer::new(router);
    for hash in [STORE_HASH, sibling_path] {
        let resp = server
            .get(&format!("/cache/test-cache/nar/{hash}.nar.zst"))
            .await;
        resp.assert_status_ok();
        assert_eq!(resp.as_bytes().as_ref(), empty_directory_nar().as_slice());
    }
    server
        .get(&format!("/cache/test-cache/nar/{other_cache_path}.nar.zst"))
        .await
        .assert_status_not_found();
    db.finish().await;
}

#[tokio::test]
async fn two_content_variants_are_served_from_their_own_cache_objects() {
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
    let (first, second) = (CacheId::now_v7(), CacheId::now_v7());
    for (id, name) in [(first, "first-cache"), (second, "second-cache")] {
        gradient_entity::cache::Model {
            public: true,
            ..gradient_fixtures::rows::cache_with_id(id, name, owner.id)
        }
        .into_active_model()
        .insert(&connection)
        .await
        .unwrap();
    }
    let second_key = format!("{STORE_HASH}-{FILE_HASH_NIX32}");
    insert_signed_path(&connection, first, STORE_HASH, "sha256:first").await;
    insert_signed_path_with_object_storage_key(
        &connection,
        second,
        STORE_HASH,
        &second_key,
        &format!("sha256:{FILE_HASH_NIX32}"),
    )
    .await;
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage.put(STORE_HASH, blob()).await.unwrap();
    nar_storage
        .put(&second_key, empty_directory_nar())
        .await
        .unwrap();
    let state = TestState::new()
        .web_db(connection)
        .nar_storage(nar_storage)
        .build();
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));

    let server = TestServer::new(router);
    for (cache, expected) in [
        ("first-cache", blob()),
        ("second-cache", empty_directory_nar()),
    ] {
        let resp = server
            .get(&format!("/cache/{cache}/nar/{STORE_HASH}.nar.zst"))
            .await;
        resp.assert_status_ok();
        assert_eq!(resp.as_bytes().as_ref(), expected.as_slice(), "{cache}");
    }
    db.finish().await;
}

#[tokio::test]
async fn public_cache_serves_signed_content_variant_on_narinfo_nar_single_file_and_directory_listing_when_no_subscribed_project_trusts_producer()
 {
    use gradient_fixtures::store_path_content_variants::{
        UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
    };

    let cli = test_cli();
    let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
    readers
        .db
        .execute("UPDATE cache SET public = true, active = true")
        .await;
    let untrusted_hash_nar_object_storage_keys = readers
        .db
        .texts(&format!(
            "SELECT nar_object_storage_key FROM cached_path WHERE hash = '{UNTRUSTED}'"
        ))
        .await;
    let nar_storage = NarStore::local(&cli.server.base_dir).expect("create test NarStore");
    nar_storage
        .put(
            &untrusted_hash_nar_object_storage_keys[0],
            gradient_test_support::cache_fixture::create_temp_directory_with_one_file_and_get_zstd_compressed_nar(
                "hello", b"hi",
            )
            .await,
        )
        .await
        .unwrap();
    let state = TestState::new()
        .web_db(readers.db.connection())
        .worker_db(readers.db.connection())
        .cache_db(readers.db.connection())
        .nar_storage(nar_storage)
        .build();
    let router = gradient_web::create_router(state)
        .expect("router")
        .layer(MockConnectInfo(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ));
    let server = TestServer::new(router);
    let cache = format!("cache-{}", readers.cache_one.into_inner().simple());

    let mut route_url_and_http_status_pairs = Vec::new();
    for route in [
        format!("/cache/{cache}/{UNTRUSTED}.narinfo"),
        format!("/cache/{cache}/nar/{UNTRUSTED}.nar.zst"),
        format!("/cache/{cache}/serve/{UNTRUSTED}/hello"),
        format!("/cache/{cache}/ls/{UNTRUSTED}"),
    ] {
        let status = server.get(&route).await.status_code();
        route_url_and_http_status_pairs.push((route, status));
    }

    assert!(
        route_url_and_http_status_pairs
            .iter()
            .all(|(_, status)| *status == axum::http::StatusCode::OK),
        "Cache C1 signed the only variant of this hash. Every route served it. Answers: {answers:?}.",
        answers = route_url_and_http_status_pairs
    );
    drop(server);
    readers.db.finish().await;
}

#[tokio::test]
async fn build_trace_realisation_route_returns_output_path_for_each_output_with_cache_signature_in_two_caches_including_write_only_subscription_and_untrusted_producer()
 {
    use gradient_fixtures::store_path_content_variants::{
        BUILD_A, SHARED, UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
    };

    let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
    let temporary_secrets_dir = tempfile::TempDir::new().expect("Secrets directory");
    let cache_signing_key_encryption_key_file_path = temporary_secrets_dir.path().join("crypt");
    std::fs::write(
        &cache_signing_key_encryption_key_file_path,
        "this-is-a-32-byte-crypt-key!!!!",
    )
    .expect("the secret is written");
    let cache_signing_key_encryption_key_file_path = cache_signing_key_encryption_key_file_path
        .to_string_lossy()
        .into_owned();
    let (private_key, public_key) =
        gradient_sources::generate_signing_key(&cache_signing_key_encryption_key_file_path)
            .expect("Key pair");
    let derivation = readers.db.shared_build(BUILD_A).await.derivation;
    readers
        .db
        .execute(&format!(
            "UPDATE cache SET private_key = '{private_key}', public_key = '{public_key}', public = true, active = true; \
             UPDATE project_cache SET mode = {write_only} WHERE cache = '{cache_two}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache_two}', convert_to('signature', 'UTF8'), 0, now() \
             FROM cached_path WHERE hash = '{UNTRUSTED}'; \
             INSERT INTO derivation_output (id, derivation, name, hash, package, path_known_after_build, created_at) \
             VALUES (uuidv7(), '{derivation}', 'out', '{SHARED}', 'shared', true, now()), \
                    (uuidv7(), '{derivation}', 'bad', '{UNTRUSTED}', 'untrusted', true, now())",
            write_only = sea_orm::ActiveEnum::to_value(
                &gradient_entity::project_cache::CacheSubscriptionMode::WriteOnly
            ),
            cache_two = readers.cache_two,
        ))
        .await;
    let derivation_file_name = readers
        .db
        .texts(&format!(
            "SELECT hash || '-' || name || '.drv' FROM derivation WHERE id = '{derivation}'"
        ))
        .await
        .remove(0);
    let state = TestState::new()
        .web_db(readers.db.connection())
        .worker_db(readers.db.connection())
        .cache_db(readers.db.connection())
        .cli(|cli| cli.secrets.crypt_file = cache_signing_key_encryption_key_file_path.clone())
        .build();
    let server = TestServer::new(gradient_web::create_router(state).expect("Router"));

    let mut expected_status_and_out_path_for_each_realisation_request = Vec::new();
    for (cache, output) in [
        (readers.cache_one, "out"),
        (readers.cache_two, "out"),
        (readers.cache_two, "bad"),
        (readers.cache_one, "bad"),
    ] {
        let cache = format!("cache-{}", cache.into_inner().simple());
        let resp = server
            .get(&format!(
                "/cache/{cache}/build-trace-v2/{derivation_file_name}/{output}.doi"
            ))
            .await;
        let output_store_path = (resp.status_code() == axum::http::StatusCode::OK)
            .then(|| {
                resp.json::<serde_json::Value>()["outPath"]
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten();
        expected_status_and_out_path_for_each_realisation_request
            .push((resp.status_code(), output_store_path));
    }

    let c1_untrusted_output_build_trace_status_and_output_path =
        expected_status_and_out_path_for_each_realisation_request.pop();
    assert_eq!(
        c1_untrusted_output_build_trace_status_and_output_path,
        Some((
            axum::http::StatusCode::OK,
            Some(format!("{UNTRUSTED}-untrusted"))
        )),
        "Cache C1 can sign untrusted outputs and serve their realisation."
    );
    assert_eq!(
        expected_status_and_out_path_for_each_realisation_request,
        [
            (axum::http::StatusCode::OK, Some(format!("{SHARED}-shared"))),
            (axum::http::StatusCode::OK, Some(format!("{SHARED}-shared"))),
            (
                axum::http::StatusCode::OK,
                Some(format!("{UNTRUSTED}-untrusted"))
            ),
        ],
        "Each cache serves the realisation of every output that it holds a signature for. Cache C2 serves the untrusted output from its own stored signature, although its only subscription is WriteOnly and cannot sign."
    );
    drop(server);
    readers.db.finish().await;
}
