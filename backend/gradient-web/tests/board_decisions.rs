/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_test_support::fixtures::user;
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

fn superuser() -> gradient_entity::user::Model {
    gradient_entity::user::Model {
        superuser: true,
        ..user()
    }
}

fn with_user(
    db: MockDatabase,
    session_id: SessionId,
    caller: gradient_entity::user::Model,
) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![caller]])
}

#[tokio::test]
async fn assign_decisions_rejects_non_superuser() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        user(),
    );

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/jobs/decisions")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn assign_decisions_superuser_returns_empty_ring() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_user(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        superuser(),
    );

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/board/jobs/decisions")
        .add_header("authorization", format!("Bearer {}", token))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"].as_array().map(Vec::len), Some(0));
}
