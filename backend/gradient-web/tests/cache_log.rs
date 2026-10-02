/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_DRV_FILENAME, cache_with_completed_build_in_cache,
    cache_with_completed_build_not_in_cache, cache_with_failed_build_only,
    cache_with_two_completed_builds, cache_with_unknown_derivation,
    private_cache_with_completed_build_in_cache,
};
use gradient_web::create_router;
use std::sync::Arc;

#[tokio::test]
async fn log_returns_text_for_completed_build_in_cache() {
    let (state, expected_log) = cache_with_completed_build_in_cache().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
        ))
        .await;
    resp.assert_status_ok();
    assert!(
        resp.header("content-type")
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    assert_eq!(resp.text(), expected_log);
}

#[tokio::test]
async fn log_404_when_build_not_linked_to_cache() {
    let state = cache_with_completed_build_not_in_cache().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
        ))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn log_404_when_only_failed_builds_exist() {
    let state = cache_with_failed_build_only().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
        ))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn log_404_for_unknown_drv_filename() {
    let state = cache_with_unknown_derivation().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let unknown_drv = "cccccccccccccccccccccccccccccccc-nothere.drv";
    let resp = server
        .get(&format!("/cache/{FIXTURE_CACHE_NAME}/log/{unknown_drv}"))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn log_returns_most_recent_when_multiple_completed_builds_exist() {
    let (state, expected_log) = cache_with_two_completed_builds().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
        ))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.text(), expected_log);
}

#[tokio::test]
async fn private_cache_log_requires_auth() {
    let state = private_cache_with_completed_build_in_cache().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .get(&format!(
            "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
        ))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
}
