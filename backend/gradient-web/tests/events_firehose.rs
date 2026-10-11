/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::rows::superuser;
use gradient_test_support::web::{make_test_server, signed_in, signed_in_as};

#[tokio::test]
async fn a_non_superuser_is_refused_the_firehose() {
    let (db, auth) = signed_in();
    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/metrics/events")
        .add_header("authorization", auth)
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_superuser_without_an_upgrade_is_told_to_upgrade() {
    let (db, auth) = signed_in_as(superuser());
    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/metrics/events")
        .add_header("authorization", auth)
        .await;
    assert!(res.status_code().is_client_error(), "{}", res.status_code());
    assert_ne!(res.status_code(), axum::http::StatusCode::FORBIDDEN);
}
