/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests: `?json` flag on text-format cache endpoints.

use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use test_support::cache_fixture::{FIXTURE_CACHE_NAME, public_cache_state};
use web::create_router;

#[test]
fn nix_cache_info_json_returns_object_with_pascal_case_keys() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state))).unwrap();

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"))
            .add_query_param("json", "")
            .await;
        resp.assert_status_ok();
        assert_eq!(
            resp.header("content-type").to_str().unwrap(),
            "application/json"
        );
        let body: Value = resp.json();
        assert_eq!(body["StoreDir"], "/nix/store");
        assert_eq!(body["WantMassQuery"], true);
        assert!(body["Priority"].is_number());
    });
}

#[test]
fn nix_cache_info_no_json_returns_text() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state))).unwrap();

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"))
            .await;
        resp.assert_status_ok();
        assert_eq!(
            resp.header("content-type").to_str().unwrap(),
            "text/x-nix-cache-info"
        );
        assert!(resp.text().contains("StoreDir: /nix/store"));
    });
}
