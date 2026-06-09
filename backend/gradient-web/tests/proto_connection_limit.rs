/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! End-to-end check that the proto upgrade route honours
//! `max_proto_connections` (issue #89).
//!
//! Builds the same wiring `gradient_web::create_router` produces for the `/proto`
//! route - `proto_router` + `Extension<Arc<Scheduler>>` +
//! `Extension<Arc<ProtoLimiter>>` - so the test can pre-acquire the only
//! configured permit and observe the rejection shape (`503` + `Retry-After`).
//! The unit tests in `gradient_proto::handler::limiter` cover the semaphore semantics
//! themselves; this test verifies the handler is actually consulting them.

use std::sync::Arc;

use axum::extract::Extension;
use axum_test::TestServer;
use http::header;
use gradient_proto::{ProtoLimiter, proto_router};
use gradient_scheduler::Scheduler;
use sea_orm::{DatabaseBackend, MockDatabase};
use gradient_test_support::state::test_state;

fn upgrade_request(server: &TestServer) -> axum_test::TestRequest {
    server
        .get("/proto")
        .add_header(header::CONNECTION, "upgrade")
        .add_header(header::UPGRADE, "websocket")
        .add_header(header::SEC_WEBSOCKET_VERSION, "13")
        .add_header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
}

fn make_server(limiter: Arc<ProtoLimiter>) -> TestServer {
    let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    let app = proto_router()
        .with_state(Arc::clone(&state))
        .layer(Extension(scheduler))
        .layer(Extension(limiter));
    // Real HTTP transport: in-memory transport rejects WS-shaped requests with
    // 426 inside the `WebSocketUpgrade` extractor before the handler body
    // runs, which would mask the limiter behaviour we're testing.
    TestServer::builder().http_transport().build(app)
}

#[test]
fn upgrade_rejected_with_503_and_retry_after_when_limit_exhausted() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let limiter = Arc::new(ProtoLimiter::new(1));
        let _hold = limiter.try_acquire().expect("first slot must be free");
        assert_eq!(limiter.in_use(), 1);

        let server = make_server(Arc::clone(&limiter));
        let res = upgrade_request(&server).await;

        res.assert_status(http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            res.header(header::RETRY_AFTER),
            "10",
            "503 must advertise a retry-after",
        );
    });
}

#[test]
fn upgrade_proceeds_past_limiter_when_slot_is_free() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let limiter = Arc::new(ProtoLimiter::new(1));
        let server = make_server(Arc::clone(&limiter));
        let res = upgrade_request(&server).await;

        // The in-memory transport doesn't carry the upgrade through to a real
        // WebSocket, so we don't check for `101` exactly - but we do check
        // that the limiter let the request *past* the rejection branch (i.e.
        // the response is not the 503 we'd see when exhausted).
        assert_ne!(
            res.status_code(),
            http::StatusCode::SERVICE_UNAVAILABLE,
            "fresh limiter must not reject the upgrade",
        );
    });
}

#[test]
fn slot_is_released_for_subsequent_upgrades_after_drop() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let limiter = Arc::new(ProtoLimiter::new(1));
        let hold = limiter.try_acquire().expect("first slot must be free");
        let server = make_server(Arc::clone(&limiter));

        upgrade_request(&server)
            .await
            .assert_status(http::StatusCode::SERVICE_UNAVAILABLE);

        drop(hold);
        let res = upgrade_request(&server).await;
        assert_ne!(
            res.status_code(),
            http::StatusCode::SERVICE_UNAVAILABLE,
            "dropping the prior permit must let the next upgrade through",
        );
    });
}
