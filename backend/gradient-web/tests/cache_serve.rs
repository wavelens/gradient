/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, private_cache_with_nar, public_cache_with_nar,
};
use gradient_web::create_router;
use std::sync::Arc;

#[test]
fn serve_returns_file_bytes() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{FIXTURE_PATH_HASH}/bin/hello"
            ))
            .await;
        resp.assert_status_ok();
        assert_eq!(resp.as_bytes().to_vec(), b"hi");
    });
}

#[test]
fn serve_returns_tar_zst_for_directory() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{FIXTURE_PATH_HASH}/bin"
            ))
            .await;
        resp.assert_status_ok();
        assert_eq!(
            resp.header("content-type").to_str().unwrap(),
            "application/zstd"
        );
        let body = resp.as_bytes().to_vec();
        assert_eq!(&body[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    });
}

#[test]
fn serve_unknown_path_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{FIXTURE_PATH_HASH}/does/not/exist"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn serve_symlink_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{FIXTURE_PATH_HASH}/bin/link"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn serve_unknown_hash_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let unknown = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{unknown}/bin/hello"
            ))
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn private_cache_serve_requires_auth() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = private_cache_with_nar().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .get(&format!(
                "/cache/{FIXTURE_CACHE_NAME}/serve/{FIXTURE_PATH_HASH}/bin/hello"
            ))
            .await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
    });
}
