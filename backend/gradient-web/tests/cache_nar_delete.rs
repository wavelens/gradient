/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_empty_nars,
};
use gradient_web::create_router;
use std::sync::Arc;

#[test]
fn delete_unauthenticated_returns_403() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_empty_nars().await;
        let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

        let resp = server
            .delete(&format!(
                "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}"
            ))
            .await;
        resp.assert_status(StatusCode::FORBIDDEN);
    });
}
