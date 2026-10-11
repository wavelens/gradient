/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `MockDatabase` cannot model unique-violation rollbacks. The race between the pre-check SELECT
//! and the INSERT is not provable with mocks.

use gradient_entity::{ids::*, project, project_user};
use gradient_fixtures::rows::{project_member, project_named};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use sea_orm::MockExecResult;
use serde_json::{Value, json};
use uuid::Uuid;

fn temp_crypt_secret_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    path.to_string_lossy().into_owned()
}

fn project_user_row(project_id: ProjectId) -> project_user::Model {
    project_user::Model {
        project: project_id,
        ..project_member(BASE_ROLE_ADMIN_ID)
    }
}

#[tokio::test]
async fn put_project_returns_already_exists_via_pre_check() {
    let (db, auth) = signed_in();

    let db = db.append_query_results([vec![project_named("dup")]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", auth)
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
}

#[tokio::test]
async fn put_project_creates_project_and_admin_membership() {
    let (db, auth) = signed_in();
    let inserted = project_named("fresh");
    let membership = project_user_row(inserted.id);

    let db = db
        .append_query_results::<project::Model, _, _>([Vec::<project::Model>::new()])
        .append_query_results([vec![inserted]])
        .append_query_results([vec![membership]])
        .append_query_results([Vec::<gradient_entity::team::Model>::new()])
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

    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_secret_file())
        .server();
    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", auth)
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
}
