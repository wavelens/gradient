/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every request consumes three auth reads first: session by jti, session
//! update, user. A public project then costs one more read (no membership).

#![expect(clippy::unwrap_used, reason = "test assertions")]

use gradient_entity::project;
use gradient_test_support::fixtures::{self, user};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::{MProject, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn public_project() -> project::Model {
    project::Model {
        public: true,
        ..fixtures::project()
    }
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

#[test]
fn starring_an_unknown_project_is_not_found() {
    run(async {
        let session_id = SessionId::now_v7();
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([Vec::<MProject>::new()]);
        let server = make_test_server(db.into_connection());

        let res = server
            .put("/api/v1/user/stars/projects/nope")
            .add_header("Authorization", bearer(session_id))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn starring_twice_is_fine() {
    run(async {
        let session_id = SessionId::now_v7();
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![public_project()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]);
        let server = make_test_server(db.into_connection());

        let res = server
            .put("/api/v1/user/stars/projects/test-project")
            .add_header("Authorization", bearer(session_id))
            .await;

        res.assert_status_ok();
        let body: Json = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], true);
    });
}

#[test]
fn unstarring_twice_is_fine() {
    run(async {
        let session_id = SessionId::now_v7();
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![public_project()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]);
        let server = make_test_server(db.into_connection());

        let res = server
            .delete("/api/v1/user/stars/projects/test-project")
            .add_header("Authorization", bearer(session_id))
            .await;

        res.assert_status_ok();
        let body: Json = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], false);
    });
}

#[test]
fn stars_list_groups_by_kind() {
    run(async {
        let session_id = SessionId::now_v7();
        let name = |n: &str| BTreeMap::from([("name", Value::from(n))]);
        let task = |p: &str, t: &str| {
            BTreeMap::from([("project", Value::from(p)), ("name", Value::from(t))])
        };
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![name("infra")]])
            .append_query_results([vec![task("infra", "hosts")]])
            .append_query_results([vec![name("main")]]);
        let server = make_test_server(db.into_connection());

        let res = server
            .get("/api/v1/user/stars")
            .add_header("Authorization", bearer(session_id))
            .await;

        res.assert_status_ok();
        let body: Json = res.json();
        assert_eq!(body["message"]["projects"], serde_json::json!(["infra"]));
        assert_eq!(
            body["message"]["tasks"],
            serde_json::json!([{ "project": "infra", "task": "hosts" }])
        );
        assert_eq!(body["message"]["caches"], serde_json::json!(["main"]));
    });
}
