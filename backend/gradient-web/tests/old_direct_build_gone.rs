/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_test_support::state::TestState;

#[tokio::test]
async fn post_builds_returns_404() {
    let server = TestState::new().server();

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
}

#[tokio::test]
async fn get_recent_direct_builds_returns_404() {
    let server = TestState::new().server();

    let response = server.get("/api/v1/builds/direct/recent").await;

    assert_eq!(
        response.status_code(),
        axum::http::StatusCode::NOT_FOUND,
        "legacy GET /builds/direct/recent must 404, got {}",
        response.status_code()
    );
}
