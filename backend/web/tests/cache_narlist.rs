/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, private_cache_with_nar, public_cache_with_nar,
};
use web::create_router;

#[test]
fn ls_returns_v1_tree_with_null_offsets() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/ls/{FIXTURE_PATH_HASH}"))
            .await;
        resp.assert_status_ok();
        let body: Value = resp.json();
        assert_eq!(body["version"], 1);
        let root = &body["root"];
        assert_eq!(root["type"], "directory");
        let bin = &root["entries"]["bin"];
        assert_eq!(bin["type"], "directory");
        let hello = &bin["entries"]["hello"];
        assert_eq!(hello["type"], "regular");
        assert!(hello["narOffset"].is_null());
        assert_eq!(hello["size"].as_u64().unwrap(), 2);
        assert!(hello.get("executable").is_none(), "non-exec omits the field");

        let exec = &bin["entries"]["exec"];
        assert_eq!(exec["type"], "regular");
        assert_eq!(exec["executable"], true);

        let link = &bin["entries"]["link"];
        assert_eq!(link["type"], "symlink");
        assert_eq!(link["target"], "hello");
    });
}

#[test]
fn ls_unknown_hash_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let unknown = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/ls/{unknown}"))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn private_cache_ls_requires_auth() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = private_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/ls/{FIXTURE_PATH_HASH}"))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
    });
}
