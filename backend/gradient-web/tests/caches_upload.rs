/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/caches/{cache}/nars`.

use axum::http::StatusCode;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use std::sync::Arc;
use gradient_test_support::cache_fixture::{FIXTURE_CACHE_NAME, public_cache_empty_nars};
use gradient_web::create_router;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn upload_unauthenticated_returns_403() {
    rt().block_on(async {
        let state = public_cache_empty_nars().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));
        let form = MultipartForm::new()
            .add_part("narinfo", Part::text("{}"))
            .add_part("nar", Part::bytes(vec![1u8, 2, 3]));
        let resp = server
            .post(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
            .multipart(form)
            .await;
        resp.assert_status(StatusCode::FORBIDDEN);
    });
}

// Needs real-DB harness; MockDatabase cannot satisfy the full write path.
#[test]
#[ignore]
fn upload_writer_creates_cached_path_and_signature() {}

#[test]
#[ignore]
fn upload_size_mismatch_returns_400() {}
