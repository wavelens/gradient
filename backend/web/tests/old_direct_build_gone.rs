/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression for the build-request rework: the legacy direct-build
//! endpoints (`POST /api/v1/builds` multipart upload, `GET
//! /api/v1/builds/direct/recent`) were replaced by the
//! `/api/v1/build-requests/*` flow and must no longer be routable.
//!
//! Uses manual Tokio runtimes because `#[tokio::test]` expands to
//! `::gradient_core::…` which clashes with the local `core` crate name.

use axum_test::TestServer;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use web::create_router;

fn make_state() -> Arc<ServerState> {
    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: std::sync::Arc::new(
            gradient_core::types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
        ),
        log_storage: Arc::new(NoopLogStorage),        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

#[test]
fn post_builds_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = TestServer::new(create_router(make_state()));

        let response = server
            .post("/api/v1/builds")
            .add_header("Content-Type", "multipart/form-data; boundary=----abc")
            .bytes(b"--\r\n".as_slice().into())
            .await;

        assert_eq!(
            response.status_code(),
            axum::http::StatusCode::NOT_FOUND,
            "legacy POST /builds must 404, got {}",
            response.status_code()
        );
    });
}

#[test]
fn get_recent_direct_builds_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = TestServer::new(create_router(make_state()));

        let response = server.get("/api/v1/builds/direct/recent").await;

        assert_eq!(
            response.status_code(),
            axum::http::StatusCode::NOT_FOUND,
            "legacy GET /builds/direct/recent must 404, got {}",
            response.status_code()
        );
    });
}
