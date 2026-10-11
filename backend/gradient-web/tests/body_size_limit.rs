/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use gradient_web::create_router;
use std::sync::Arc;
use uuid::Uuid;

fn make_state_with_limits(max_request_size: usize) -> Arc<ServerState> {
    TestState::new()
        .cli(|cli| {
            cli.http.max_request_size =
                std::num::NonZeroUsize::new(max_request_size).expect("non-zero")
        })
        .build()
}

#[tokio::test]
async fn webhook_body_over_limit_returns_413() {
    let state = make_state_with_limits(1024);
    let router = create_router(state).expect("router");
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
}

#[tokio::test]
async fn webhook_body_within_limit_reaches_handler() {
    let state = make_state_with_limits(1024);
    let router = create_router(state).expect("router");
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
}

#[tokio::test]
async fn blob_upload_route_uses_higher_limit() {
    let state = make_state_with_limits(1024);
    let router = create_router(state).expect("router");
    let server = TestServer::new(router);

    let body = vec![b'x'; 16 * 1024];
    let session_id = Uuid::now_v7();

    let response = server
        .post(&format!("/api/v1/build-requests/{}/blobs", session_id))
        .add_header("Content-Type", "multipart/form-data; boundary=----abc")
        .bytes(body.into())
        .await;

    assert_ne!(
        response.status_code(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        "blob-upload route must not enforce the smaller global limit, got {}",
        response.status_code()
    );
}

/// The request must be authenticated. The `authorize` middleware is rejecting an anonymous
/// `/build-requests/*` request before the body limit is evaluated.
#[tokio::test]
async fn source_chunk_over_source_limit_returns_413() {
    let (db, auth) = signed_in();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.http.max_source_upload_size =
                std::num::NonZeroUsize::new(32 * 1024).expect("non-zero");
        })
        .server();

    let over = vec![b'x'; 64 * 1024];
    let response = server
        .put("/api/v1/build-requests/source/testupload/chunk?offset=0")
        .add_header("authorization", auth)
        .add_header("Content-Type", "application/octet-stream")
        .bytes(over.into())
        .await;
    assert_eq!(
        response.status_code(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        "chunk over max_source_upload_size must be 413'd, got {}",
        response.status_code()
    );
}
