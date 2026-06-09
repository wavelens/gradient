/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests: `?json` flag on text-format cache endpoints.

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::Value;
use std::sync::Arc;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, private_cache_state, public_cache_state,
    public_cache_with_narinfo,
};
use gradient_web::create_router;

#[test]
fn nix_cache_info_json_returns_object_with_pascal_case_keys() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

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
        let server = TestServer::new(create_router(Arc::clone(&state)));

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

#[test]
fn gradient_cache_info_json_returns_object() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/gradient-cache-info"))
            .add_query_param("json", "")
            .await;
        resp.assert_status_ok();
        let body: Value = resp.json();
        assert!(body["GradientVersion"].is_string());
        assert!(body["GradientUrl"].is_string());
    });
}

#[test]
fn gradient_cache_info_no_json_returns_text() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/gradient-cache-info"))
            .await;
        resp.assert_status_ok();
        assert!(resp.text().contains("GradientVersion:"));
        assert!(resp.text().contains("GradientUrl:"));
    });
}

#[test]
fn narinfo_json_returns_object_with_pascal_case_keys() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_narinfo().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

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
    });
}

#[test]
fn private_cache_requires_auth() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = private_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);

        let state = private_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));
        let resp = server
            .get(&format!("/cache/{FIXTURE_CACHE_NAME}/gradient-cache-info"))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);

        let state = private_cache_state().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));
        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/{FIXTURE_PATH_HASH}.narinfo"
            ))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
    });
}
