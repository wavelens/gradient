/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression tests for `GET /api/v1/board/jobs/decisions`.
//!
//! The endpoint is superuser-only and depends on `Extension<MUser>`, so it must
//! live on the authenticated tier. It was originally mounted on the optional-auth
//! tier (which only supplies `MaybeUser`), making the extractor fail with `500`
//! on every request - the Live Jobs "incl. rejected" table then swallowed the
//! error and stayed empty (#419).

use gradient_test_support::fixtures::user;
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

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

#[test]
fn dispatch_decisions_rejects_non_superuser() {
    run(async {
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
    });
}

#[test]
fn dispatch_decisions_superuser_returns_empty_ring() {
    run(async {
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
    });
}
