/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_available_false,
    public_cache_available_true,
};
use gradient_web::create_router;
use serde_json::Value;
use std::sync::Arc;

#[tokio::test]
async fn available_returns_true_when_signature_present() {
    let state = public_cache_available_true().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/available"
        ))
        .add_query_param("hash", FIXTURE_PATH_HASH)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["error"], Value::Bool(false));
    assert_eq!(body["message"]["available"], Value::Bool(true));
}

#[tokio::test]
async fn available_returns_false_when_no_cached_path() {
    let state = public_cache_available_false().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/available"
        ))
        .add_query_param("hash", FIXTURE_PATH_HASH)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["error"], Value::Bool(false));
    assert_eq!(body["message"]["available"], Value::Bool(false));
}
