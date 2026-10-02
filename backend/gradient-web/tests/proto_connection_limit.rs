/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::extract::Extension;
use axum_test::TestServer;
use gradient_proto::proto_router;
use gradient_scheduler::Scheduler;
use gradient_test_support::state::test_state;
use gradient_wire::ProtoLimiter;
use http::header;
use sea_orm::{DatabaseBackend, MockDatabase};

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
        .layer(Extension(limiter))
        .layer(Extension(gradient_proto::SessionsHandle::new()));
    // The in-memory transport is rejecting WS-shaped requests with 426 before the handler body
    // is running. That would mask the limiter behaviour under test.
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
