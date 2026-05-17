/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use std::sync::Arc;
use test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_DRV_FILENAME, cache_with_completed_build_in_cache,
    cache_with_completed_build_not_in_cache, cache_with_failed_build_only,
};
use web::create_router;

#[test]
fn log_returns_text_for_completed_build_in_cache() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (state, expected_log) = cache_with_completed_build_in_cache().await;
        let server = TestServer::new(create_router(Arc::clone(&state))).unwrap();

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
            ))
            .await;
        resp.assert_status_ok();
        assert!(resp
            .header("content-type")
            .to_str()
            .unwrap()
            .starts_with("text/plain"));
        assert_eq!(resp.text(), expected_log);
    });
}

#[test]
fn log_404_when_build_not_linked_to_cache() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = cache_with_completed_build_not_in_cache().await;
        let server = TestServer::new(create_router(Arc::clone(&state))).unwrap();

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn log_404_when_only_failed_builds_exist() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = cache_with_failed_build_only().await;
        let server = TestServer::new(create_router(Arc::clone(&state))).unwrap();

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/log/{FIXTURE_DRV_FILENAME}"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}
