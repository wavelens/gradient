/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_with_one_nar,
    public_cache_with_path_no_signature,
};
use gradient_web::create_router;
use serde_json::Value;
use std::sync::Arc;

#[tokio::test]
async fn show_returns_full_detail() {
    let state = public_cache_with_one_nar().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}"
        ))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["error"], Value::Bool(false));
    let detail = &body["message"];
    assert_eq!(detail["hash"], FIXTURE_PATH_HASH);
    assert!(detail["store_path"].is_string());
    assert!(detail["package"].is_string());
    assert!(detail["references"].is_array());
    assert!(detail["fetch_count"].is_number());
    assert!(detail["signed"].is_boolean());
}

#[tokio::test]
async fn cache_nar_detail_shows_only_the_content_variant_that_cache_serves() {
    use gradient_fixtures::store_path_content_variants::{
        FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
    };
    use gradient_test_support::state::TestState;

    let first_reference = "44444444444444444444444444444444-first";
    let second_reference = "55555555555555555555555555555555-second";
    let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
    readers
        .db
        .execute(&format!(
            "UPDATE cache SET public = true, active = true; \
             UPDATE cached_path SET \"references\" = '{first_reference}' \
             WHERE nar_object_storage_key = '{FIRST_KEY}'; \
             UPDATE cached_path SET \"references\" = '{second_reference}' \
             WHERE nar_object_storage_key = '{SECOND_KEY}'"
        ))
        .await;
    let state = TestState::new()
        .web_db(readers.db.connection())
        .worker_db(readers.db.connection())
        .cache_db(readers.db.connection())
        .build();
    let server = TestServer::new(create_router(state).expect("router"));

    let resp = server
        .get(&format!(
            "/api/v1/caches/cache-{}/nars/{SHARED}",
            readers.cache_two.into_inner().simple()
        ))
        .await;

    resp.assert_status_ok();
    let body: Value = resp.json();
    let detail = &body["message"];
    assert!(
        detail["nar_hash"] == "sha256:second"
            && detail["references"] == serde_json::json!([second_reference]),
        "Cache C2 serves only the variant of project B, so the NAR detail carries the NAR hash and the references of that variant: {detail}"
    );
    drop(server);
    readers.db.finish().await;
}

#[tokio::test]
async fn show_404_when_signature_missing() {
    let state = public_cache_with_path_no_signature().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}"
        ))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}
