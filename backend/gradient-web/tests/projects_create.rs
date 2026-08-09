/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `PUT /api/v1/projects`.
//!
//! Two paths are exercised:
//!   * the in-handler pre-check that rejects a name already taken (lock-in
//!     regression around the 409 response shape);
//!   * the happy-path transactional flow where the pre-check is empty, both
//!     `project` and `project_user` insert, and the tx commits.
//!
//! `MockDatabase` cannot model unique-violation rollbacks - `begin()` and
//! `commit()` succeed unconditionally. The race between the pre-check SELECT
//! and the INSERT is therefore a SeaORM transaction-semantics trust boundary,
//! not something we can prove with mocks. The two tests here are the
//! strongest sequencing guarantee mocks can provide.

use gradient_entity::{ids::*, project, project_user};
use gradient_test_support::fixtures::{test_date, user, user_id};
use gradient_test_support::web::{
    live_session, make_test_server, make_test_server_with, make_token,
};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

fn temp_crypt_secret_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    path.to_string_lossy().into_owned()
}

fn project_row(name: &str) -> project::Model {
    project::Model {
        id: ProjectId::now_v7(),
        name: name.to_string(),
        display_name: format!("{} display", name),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn project_user_row(project_id: ProjectId) -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::now_v7(),
        project: project_id,
        user: user_id(),
        role: BASE_ROLE_ADMIN_ID,
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

#[test]
fn put_project_returns_already_exists_via_pre_check() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project_row("dup")]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/projects")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "dup",
                "display_name": "dup",
                "description": "",
                "public": false,
            }))
            .await;

        res.assert_status(axum::http::StatusCode::CONFLICT);
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert_eq!(body["code"], "already_exists");
    });
}

#[test]
fn put_project_creates_project_and_admin_membership() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let inserted = project_row("fresh");
        let membership = project_user_row(inserted.id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results::<project::Model, _, _>([Vec::<project::Model>::new()])
            .append_query_results([vec![inserted]])
            .append_query_results([vec![membership]])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ]);

        let server = make_test_server_with(db.into_connection(), Some(temp_crypt_secret_file()));
        let res = server
            .put("/api/v1/projects")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "fresh",
                "display_name": "Fresh",
                "description": "",
                "public": false,
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}
