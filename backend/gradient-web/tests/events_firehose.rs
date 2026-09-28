/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_test_support::fixtures::{superuser_user, user};
use gradient_test_support::web::{live_session, make_test_server_with, make_token};
use gradient_types::{MUser, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase};

fn authed_as(caller: MUser, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![caller]])
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_non_superuser_is_refused_the_firehose() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server_with(authed_as(user(), session_id).into_connection(), None);
        let res = server
            .get("/api/v1/metrics/events")
            .add_header(
                "authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;
        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn a_superuser_without_an_upgrade_is_told_to_upgrade() {
    runtime().block_on(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server_with(
            authed_as(superuser_user(), session_id).into_connection(),
            None,
        );
        let res = server
            .get("/api/v1/metrics/events")
            .add_header(
                "authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;
        assert!(res.status_code().is_client_error(), "{}", res.status_code());
        assert_ne!(res.status_code(), axum::http::StatusCode::FORBIDDEN);
    });
}
