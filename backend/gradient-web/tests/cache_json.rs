/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, private_cache_state, public_cache_state,
    public_cache_with_narinfo,
};
use gradient_web::create_router;
use serde_json::Value;
use std::sync::Arc;

#[tokio::test]
async fn nix_cache_info_json_returns_object_with_pascal_case_keys() {
    let state = public_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

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
}

#[tokio::test]
async fn nix_cache_info_no_json_returns_text() {
    let state = public_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"))
        .await;
    resp.assert_status_ok();
    assert_eq!(
        resp.header("content-type").to_str().unwrap(),
        "text/x-nix-cache-info"
    );
    assert!(resp.text().contains("StoreDir: /nix/store"));
    assert!(resp.text().contains("WantMassQuery: 1"));
}

#[tokio::test]
async fn cache_root_redirects_to_nix_cache_info() {
    let state = public_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    for path in [
        format!("/cache/{FIXTURE_CACHE_NAME}"),
        format!("/cache/{FIXTURE_CACHE_NAME}/"),
    ] {
        let resp = server.get(&path).await;
        resp.assert_status(StatusCode::FOUND);
        assert_eq!(
            resp.header("location").to_str().unwrap(),
            format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info")
        );
    }
}

#[tokio::test]
async fn narinfo_json_returns_object_with_pascal_case_keys() {
    let state = public_cache_with_narinfo().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/{FIXTURE_PATH_HASH}.narinfo"
        ))
        .add_query_param("json", "")
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(
        body["StorePath"]
            .as_str()
            .unwrap()
            .starts_with("/nix/store/")
    );
    assert!(body["URL"].as_str().unwrap().starts_with("nar/"));
    assert!(body["NarHash"].as_str().unwrap().starts_with("sha256:"));
}

#[tokio::test]
async fn private_cache_requires_auth() {
    let state = private_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    let state = private_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));
    let resp = server
        .get(&format!("/cache/{FIXTURE_CACHE_NAME}/gradient-cache-info"))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    let state = private_cache_state().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));
    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/{FIXTURE_PATH_HASH}.narinfo"
        ))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
}
