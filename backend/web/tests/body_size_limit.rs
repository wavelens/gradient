/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression for #51: oversized request bodies must be rejected with
//! 413 Payload Too Large *before* the handler can buffer them, so a 10 GB
//! webhook payload cannot exhaust server memory. The cap is configurable
//! via `--max-request-size` (default 2 MiB) and applied as a
//! `DefaultBodyLimit` layer on the API router; the per-route override on
//! `POST /api/v1/builds` raises it to `--max-direct-build-size` for
//! direct-build multipart uploads.
//!
//! Uses manual Tokio runtimes because `#[tokio::test]` expands to
//! `::gradient_core::…` which clashes with the local `core` crate name.

use axum_test::TestServer;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

fn make_state_with_limits(
    max_request_size: usize,
    max_direct_build_size: usize,
) -> Arc<ServerState> {
    let mut cli = test_cli();
    cli.limits.max_request_size = max_request_size;
    cli.limits.max_direct_build_size = max_direct_build_size;
    let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: std::sync::Arc::new(gradient_core::types::RuntimeConfig::from_cli(&cli)),
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    })
}

/// A POST whose body exceeds `max_request_size` is rejected with 413
/// before the webhook handler ever runs (so signature verification is
/// never attempted, which is exactly the OOM-prevention property we want).
#[test]
fn webhook_body_over_limit_returns_413() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = make_state_with_limits(1024, 1024 * 1024);
        let router = create_router(state);
        let server = TestServer::new(router);

        let oversized = vec![b'x'; 4096];

        let response = server
            .post("/api/v1/hooks/github")
            .add_header("X-Hub-Signature-256", "sha256=deadbeef")
            .add_header("X-GitHub-Event", "push")
            .bytes(oversized.into())
            .await;

        assert_eq!(
            response.status_code(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "body over max_request_size must be rejected with 413, got {}",
            response.status_code()
        );
    });
}

/// A body within the limit reaches the handler. The webhook then rejects
/// with 401 (invalid signature) — we don't care about that, only that the
/// body limit didn't trip.
#[test]
fn webhook_body_within_limit_reaches_handler() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = make_state_with_limits(1024, 1024 * 1024);
        let router = create_router(state);
        let server = TestServer::new(router);

        let small = vec![b'x'; 256];

        let response = server
            .post("/api/v1/hooks/github")
            .add_header("X-Hub-Signature-256", "sha256=deadbeef")
            .add_header("X-GitHub-Event", "push")
            .bytes(small.into())
            .await;

        assert_ne!(
            response.status_code(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "body within max_request_size must not be rejected with 413"
        );
    });
}

/// `POST /api/v1/builds` (direct-build multipart) gets a per-route layer
/// raising the limit to `max_direct_build_size`, so a payload that would
/// fail the global `max_request_size` is allowed through. We send the
/// request unauthenticated — it will fail with 401, but the point is that
/// a 413 response would mean the per-route override isn't in effect.
#[test]
fn direct_build_route_uses_higher_limit() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Global limit 1 KiB; per-route limit 1 MiB.
        let state = make_state_with_limits(1024, 1024 * 1024);
        let router = create_router(state);
        let server = TestServer::new(router);

        // Payload over the global limit but under the per-route limit.
        let body = vec![b'x'; 16 * 1024];

        let response = server
            .post("/api/v1/builds")
            .add_header("Content-Type", "multipart/form-data; boundary=----abc")
            .bytes(body.into())
            .await;

        assert_ne!(
            response.status_code(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "direct-build route must not enforce the smaller global limit, got {}",
            response.status_code()
        );
        // Auth middleware rejects unauthenticated requests; that's the
        // expected outcome here, distinct from a body-limit rejection.
        let _ = Uuid::nil();
    });
}
