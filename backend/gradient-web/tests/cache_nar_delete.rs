/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `DELETE /api/v1/caches/{cache}/nars/{hash}`.
//!
//! State-mutation behaviors (audit row written, blob deleted, signature gone)
//! cannot be asserted against the mock DB - they require a real Postgres
//! harness. Only the auth surface is exercised here.

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
        // The auth middleware rejects requests with no Authorization header
        // before they reach the handler, returning 403.
        resp.assert_status(StatusCode::FORBIDDEN);
    });
}

// TODO(#260): needs real-DB harness; mock cannot verify blob deletion,
// signature row removal, or audit_log insertion. The handler's control flow
// is exercised by `helpers::delete_nar_from_cache` unit coverage at the
// integration level once a real Postgres fixture is available.
#[test]
#[ignore]
fn delete_owner_removes_signature_and_writes_audit_row() {}
