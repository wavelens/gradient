/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /api/v1/caches/{cache}/nars`.

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, private_cache_for_nars, public_cache_empty_nars,
};
use web::create_router;

#[test]
fn list_empty_cache_returns_empty_items() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_empty_nars().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
            .await;
        resp.assert_status_ok();
        let body: Value = resp.json();
        assert_eq!(body["error"], Value::Bool(false));
        let items = body["message"]["items"].as_array().expect("items array");
        assert!(items.is_empty(), "expected no items, got {items:?}");
        assert!(body["message"]["total"].is_number());
        assert!(body["message"]["page"].is_number());
        assert!(body["message"]["per_page"].is_number());
    });
}

#[test]
fn list_private_cache_requires_auth() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = private_cache_for_nars().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
    });
}

#[test]
fn list_accepts_pagination_query_params() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_empty_nars().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
            .add_query_param("page", "1")
            .add_query_param("per_page", "10")
            .add_query_param("sort", "created_at")
            .add_query_param("order", "desc")
            .await;
        resp.assert_status_ok();
    });
}
