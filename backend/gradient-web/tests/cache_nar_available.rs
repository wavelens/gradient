/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /api/v1/caches/{cache}/nars/available?hash=...`.

use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_available_false,
    public_cache_available_true,
};
use gradient_web::create_router;

#[test]
fn available_returns_true_when_signature_present() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_available_true().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

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
    });
}

#[test]
fn available_returns_false_when_no_cached_path() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_available_false().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

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
    });
}
