/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project;
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{
    admin_role, live_session, project_member, task_without_cache_signing, user,
};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use sea_orm::MockDatabase;
use serde_json::{Value, json};

fn authorized_db(session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![project::Model {
            id: project_id(),
            name: "test-project".into(),
            display_name: "Test Project".into(),
            public_key: "ssh-ed25519 AAAA test".into(),
            private_key: "encrypted".into(),
            created_by: user_id(),
            created_at: test_date(),
            ..Default::default()
        }]])
        .append_query_results([vec![task_without_cache_signing()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

async fn evaluate(db: MockDatabase, session_id: SessionId, body: Value) -> axum_test::TestResponse {
    make_test_server(db.into_connection())
        .post("/api/v1/tasks/test-project/test-task/evaluate")
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&body)
        .await
}

#[tokio::test]
async fn rejects_a_commit_that_is_not_a_full_hash() {
    let session_id = SessionId::now_v7();
    let res = evaluate(
        authorized_db(session_id),
        session_id,
        json!({ "commit": "9c1a2b3" }),
    )
    .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("40-character"),
        "unexpected message: {}",
        body["message"]
    );
}

#[tokio::test]
async fn rejects_an_unparsable_attr() {
    let session_id = SessionId::now_v7();
    let res = evaluate(
        authorized_db(session_id),
        session_id,
        json!({ "attr": ".packages" }),
    )
    .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("attr"),
        "unexpected message: {}",
        body["message"]
    );
}
