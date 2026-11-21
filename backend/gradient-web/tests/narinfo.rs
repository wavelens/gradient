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
async fn a_cache_narinfo_shows_only_the_content_variant_that_cache_serves() {
    use gradient_fixtures::store_path_content_variants::{
        FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
    };

    let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
    readers
        .db
        .execute(&format!(
            "UPDATE cache SET public = true, active = true; \
             UPDATE cached_path SET \"references\" = '{FIXTURE_HASH}-first' \
             WHERE nar_object_storage_key = '{FIRST_KEY}'; \
             UPDATE cached_path SET \"references\" = '{EXAMPLE_STORED_HASH}-second' \
             WHERE nar_object_storage_key = '{SECOND_KEY}'"
        ))
        .await;
    let state = TestState::new()
        .web_db(readers.db.connection())
        .worker_db(readers.db.connection())
        .cache_db(readers.db.connection())
        .build();
    let server = TestServer::new(create_router(state).expect("router"));

    let response = server
        .get(&format!(
            "/cache/cache-{}/{SHARED}.narinfo",
            readers.cache_two.into_inner().simple()
        ))
        .await;

    response.assert_status_ok();
    let body = response.text();
    assert!(
        body.contains("NarHash: sha256:second\n")
            && body.contains(&format!("References: {EXAMPLE_STORED_HASH}-second\n"))
            && !body.contains(FIXTURE_HASH)
            && body.contains("Sig: "),
        "Cache C2 serves only the variant of project B, so its narinfo carries the NAR hash, the references and the signature of that variant:\n{body}"
    );
    drop(server);
    readers.db.finish().await;
}

fn mock_database_for_narinfo_tests_with_public_cache_deriver_and_signature_rows(
    deriver: String,
) -> sea_orm::DatabaseConnection {
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
        nar_object_storage_key: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        file_size: Some(12345),
        nar_size: 67890,
        nar_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
        deriver: Some(deriver),
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

    mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![cached_path_sig_row]])
        .into_connection()
}

#[tokio::test]
async fn narinfo_returns_server_error_when_deriver_contains_whitespace() {
    let db = mock_database_for_narinfo_tests_with_public_cache_deriver_and_signature_rows(format!(
        "/nix/store/{FIXTURE_HASH}-hello.drv {FIXTURE_HASH}-other.drv"
    ));
    let server = TestServer::new(create_router(TestState::new().web_db(db).build()).unwrap());

    let response = server
        .get(&format!("/cache/test-cache/{FIXTURE_HASH}.narinfo"))
        .await;

    response.assert_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.text();
    assert!(body.contains("Internal server error"), "{body}");
    assert!(!body.contains("other.drv"), "{body}");
}

#[tokio::test]
async fn narinfo_served_from_db_without_daemon_probe() {
    let db = mock_database_for_narinfo_tests_with_public_cache_deriver_and_signature_rows(format!(
        "/nix/store/{FIXTURE_HASH}-hello.drv"
    ));
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

/// Return 404 if `signature` is NULL. The entire privacy guarantee for `sign_outputs=false` can rest
/// on one rule. Never serve an unsigned narinfo.
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
        nar_object_storage_key: FIXTURE_HASH.into(),
        package: "hello".into(),
        file_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        file_size: Some(12345),
        nar_size: 67890,
        nar_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
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
        nar_object_storage_key: stored.into(),
        package: "hello".into(),
        file_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        file_size: Some(12345),
        nar_size: 67890,
        nar_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
        deriver: Some(format!("/nix/store/{stored}-hello.drv")),
        references: Some(format!("{stored}-hello")),
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
    let db = mock_db()
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cached_path_row]])
        .append_query_results([vec![signature_row]])
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
        body.contains(&format!("URL: nar/{original}.nar.zst\n")),
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
