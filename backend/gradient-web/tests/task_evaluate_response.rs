/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{project, role, task};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, project_member, user};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::MockDatabase;
use serde_json::{Value, json};

fn task_row() -> task::Model {
    task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::Skip,
        ..Default::default()
    }
}

fn admin_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    }
}

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
        .append_query_results([vec![task_row()]])
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
