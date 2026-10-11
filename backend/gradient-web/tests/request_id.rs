/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_test_support::state::TestState;
use uuid::Uuid;

#[tokio::test]
async fn missing_request_id_is_generated() {
    let server = TestState::new().server();
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
}

#[tokio::test]
async fn supplied_request_id_is_echoed() {
    let server = TestState::new().server();
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
}

#[tokio::test]
async fn each_request_gets_a_distinct_id() {
    let server = TestState::new().server();
    let first = server.get("/api/v1/health").await;
    let second = server.get("/api/v1/health").await;

    assert_ne!(
        first.header("x-request-id"),
        second.header("x-request-id"),
        "successive requests must get unique ids - otherwise log \
             correlation collapses across concurrent requests"
    );
}
