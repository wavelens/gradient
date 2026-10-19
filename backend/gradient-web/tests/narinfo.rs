/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum_test::TestServer;
use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
use gradient_fixtures::mock::mock_db;
use gradient_test_support::state::TestState;
use gradient_types::ids::*;
use gradient_web::create_router;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
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

const FIXTURE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn narinfo_served_from_db_without_daemon_probe() {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
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
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        deriver: Some(format!("/nix/store/{}-hello.drv", FIXTURE_HASH)),
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

    let state = TestState::new().web_db(db).build();

    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let response = server
        .get(&format!("/cache/test-cache/{}.narinfo", FIXTURE_HASH))
        .await;

    response.assert_status_ok();
    assert_eq!(
        response.header("x-cache").to_str().unwrap(),
        "HIT",
        "narinfo served from our store must report X-Cache: HIT"
    );
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
        body.contains(&format!("Deriver: {FIXTURE_HASH}-hello.drv")),
        "narinfo Deriver must be a store-path basename:\n{body}"
    );
    assert!(
        !body.contains("Deriver: /nix/store/"),
        "narinfo Deriver must not carry the /nix/store/ prefix:\n{body}"
    );
    assert!(
        !body.contains("References:"),
        "narinfo with no references must omit the References line:\n{body}"
    );
}

/// A NULL `signature` must answer 404. The whole `sign_cache=false` privacy guarantee is depending
/// on never serving an unsigned narinfo.
#[tokio::test]
async fn narinfo_returns_404_when_signature_null() {
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
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
        hash: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
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

    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![unsigned_sig_row]])
        .into_connection();

    let state = TestState::new().web_db(db).build();

    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let response = server
        .get(&format!("/cache/test-cache/{}.narinfo", FIXTURE_HASH))
        .await;

    response.assert_status_not_found();
}

#[tokio::test]
async fn a_narinfo_names_the_path_the_client_asked_for() {
    let (original, stored) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
    let cache_row = gradient_entity::cache::Model {
        id: cache_id(),
        name: "test-cache".into(),
        active: true,
        public: true,
        created_by: UserId::new(project_id().into_inner()),
        created_at: test_date(),
        ..Default::default()
    };
    let cached_path_row = gradient_entity::cached_path::Model {
        id: cached_path_id(),
        hash: stored.into(),
        package: "hello".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(12345),
        nar_size: Some(67890),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        deriver: Some(format!("/nix/store/{stored}-hello.drv")),
        created_at: test_date(),
        ..Default::default()
    };
    let signature_row = gradient_entity::cached_path_signature::Model {
        id: cached_path_sig_id(),
        cached_path: cached_path_id(),
        cache: cache_id(),
        signature: Some(vec![0x42; 64]),
        created_at: test_date(),
        ..Default::default()
    };
    let references_row = BTreeMap::from([
        ("hash", sea_orm::Value::from(stored)),
        (
            "references",
            sea_orm::Value::from(format!("{stored}-hello")),
        ),
    ]);
    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![signature_row]])
        .append_query_results([vec![references_row]])
        .into_connection();
    let state = TestState::new()
        .web_db(db.clone())
        .store_path_mapping(Arc::new(RotatedHashMapping))
        .build();

    let response = TestServer::new(create_router(state).expect("router"))
        .get(&format!("/cache/test-cache/{original}.narinfo"))
        .await;

    response.assert_status_ok();
    let body = response.text();
    assert!(
        body.contains(&format!("StorePath: /nix/store/{original}-hello\n")),
        "{body}"
    );
    assert!(
        body.contains(&format!("References: {original}-hello\n")),
        "{body}"
    );
    assert!(
        body.contains(&format!("Deriver: {original}-hello.drv\n")),
        "{body}"
    );
    let log = format!("{:?}", db.into_transaction_log());
    assert!(
        log.contains(stored),
        "the lookup uses the stored hash: {log}"
    );
}
