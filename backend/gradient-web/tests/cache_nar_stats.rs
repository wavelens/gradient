/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /api/v1/caches/{cache}/nars/stats`.

use axum_test::TestServer;
use gradient_test_support::cache_fixture::{FIXTURE_CACHE_NAME, public_cache_stats_row};
use gradient_web::create_router;
use serde_json::Value;
use std::sync::Arc;

#[test]
fn stats_returns_aggregates() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = public_cache_stats_row().await;
        let server = TestServer::new(create_router(Arc::clone(&state)));

        let resp = server
            .get(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/stats"))
            .await;
        resp.assert_status_ok();
        let body: Value = resp.json();
        assert_eq!(body["error"], Value::Bool(false));
        let stats = &body["message"];
        assert!(stats["total_nars"].is_number());
        assert!(stats["total_nar_size"].is_number());
        assert!(stats["total_file_size"].is_number());
    });
}
