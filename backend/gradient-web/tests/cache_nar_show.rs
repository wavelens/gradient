/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /api/v1/caches/{cache}/nars/{hash}`.

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_with_one_nar,
    public_cache_with_path_no_signature,
};
use gradient_web::create_router;

#[test]
fn show_returns_full_detail() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_one_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

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
    });
}

#[test]
fn show_404_when_signature_missing() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_path_no_signature().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!(
                "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}
