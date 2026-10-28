/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::{project, task_action};
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

const PATCH: &str = "diff --git a/flake.lock b/flake.lock\n--- a/flake.lock\n+++ b/flake.lock\n@@ -1 +1 @@\n-a\n+b\n";

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

async fn open(db: MockDatabase, session_id: SessionId, body: Value) -> axum_test::TestResponse {
    make_test_server(db.into_connection())
        .post("/api/v1/tasks/test-project/test-task/pull-requests")
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&body)
        .await
}

#[tokio::test]
async fn a_task_without_an_open_pr_action_answers_409() {
    let session_id = SessionId::now_v7();
    let db = authorized_db(session_id).append_query_results([Vec::<task_action::Model>::new()]);

    let res = open(
        db,
        session_id,
        json!({ "title": "update", "patch": PATCH, "commit": "a".repeat(40) }),
    )
    .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
    assert_eq!(
        res.json::<Value>()["message"],
        "the task has no open_pr action naming a Git host integration"
    );
}

#[tokio::test]
async fn a_user_without_a_commit_answers_400() {
    let session_id = SessionId::now_v7();

    let res = open(
        authorized_db(session_id),
        session_id,
        json!({ "title": "update", "patch": PATCH }),
    )
    .await;

    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>()["message"],
        "commit is required outside an exec job"
    );
}
