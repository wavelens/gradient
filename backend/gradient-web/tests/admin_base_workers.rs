/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::base_worker;
use gradient_test_support::fixtures::user;
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};

fn with_user(db: MockDatabase, session_id: SessionId, superuser: bool) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![gradient_entity::user::Model {
            superuser,
            ..user()
        }]])
}

#[tokio::test]
async fn base_workers_are_listed_for_superusers_only() {
    let session_id = SessionId::now_v7();
    let db = with_user(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        false,
    );
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/admin/base-workers")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn a_state_base_worker_cannot_be_disconnected() {
    let session_id = SessionId::now_v7();
    let db = with_user(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        true,
    )
    .append_query_results([vec![base_worker::Model {
        worker_id: "bw1".into(),
        ..Default::default()
    }]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .delete("/api/v1/admin/base-workers/bw1")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
}
