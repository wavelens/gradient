/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use gradient_test_support::cache_fixture::{FIXTURE_CACHE_NAME, public_cache_empty_nars};
use gradient_web::create_router;
use std::sync::Arc;

#[tokio::test]
async fn upload_unauthenticated_returns_403() {
    let state = public_cache_empty_nars().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));
    let form = MultipartForm::new()
        .add_part("narinfo", Part::text("{}"))
        .add_part("nar", Part::bytes(vec![1u8, 2, 3]));
    let resp = server
        .post(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
        .multipart(form)
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
}
