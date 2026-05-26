/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression for #86: every response must carry an `x-request-id` so logs
//! emitted while the request is in flight (handler, DB, spawned cleanup
//! tasks) can be correlated with a single grep. When the client supplies
//! the header - typically a reverse proxy injecting one - the server must
//! preserve it instead of minting a new id.
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
use uuid::Uuid;
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
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
    })
}

#[test]
fn missing_request_id_is_generated() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = TestServer::new(create_router(make_state()));
        let response = server.get("/api/v1/health").await;
        response.assert_status_ok();

        let value = response
            .header("x-request-id")
            .to_str()
            .expect("x-request-id is ASCII")
            .to_owned();
        assert!(
            !value.is_empty(),
            "server must mint an x-request-id when none is supplied"
        );
        Uuid::parse_str(&value).expect("auto-generated id must be a UUID");
    });
}

#[test]
fn supplied_request_id_is_echoed() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = TestServer::new(create_router(make_state()));
        let supplied = "trace-from-upstream-proxy";
        let response = server
            .get("/api/v1/health")
            .add_header("x-request-id", supplied)
            .await;
        response.assert_status_ok();

        assert_eq!(
            response.header("x-request-id"),
            supplied,
            "client-supplied x-request-id must be preserved end-to-end \
             so reverse-proxy traces stay stitched together"
        );
    });
}

#[test]
fn each_request_gets_a_distinct_id() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = TestServer::new(create_router(make_state()));
        let first = server.get("/api/v1/health").await;
        let second = server.get("/api/v1/health").await;

        assert_ne!(
            first.header("x-request-id"),
            second.header("x-request-id"),
            "successive requests must get unique ids - otherwise log \
             correlation collapses across concurrent requests"
        );
    });
}
