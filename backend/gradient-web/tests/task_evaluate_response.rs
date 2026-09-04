/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Regression test for `POST /api/v1/tasks/{project}/{task}/evaluate` (#564).
//!
//! The endpoint used to answer with the prose `"Evaluation started"` while both
//! its OpenAPI contract and the CLI (`gradient task evaluate` prints
//! `{"evaluation_id": <message>}`) expected the new evaluation's UUID, leaving
//! every API-driven caller unable to follow the run it had just started.
//!
//! `restart_failed` is the path exercised here because the normal path resolves
//! the branch head over git first; both return the same thing.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::{entry_point, evaluation, ids::*, project, project_user, role, task};
use gradient_test_support::fixtures::{commit_id, project_id, task_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

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

fn admin_membership() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        project: project_id(),
        user: user_id(),
        role: BASE_ROLE_ADMIN_ID,
    }
}

fn admin_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_db::permissions::admin_mask(),
        ..Default::default()
    }
}

fn eval_row(id: EvaluationId, status: EvaluationStatus) -> evaluation::Model {
    evaluation::Model {
        id,
        task: Some(task_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn restart_failed_answers_with_the_new_evaluation_id() {
    run(async {
        let session_id = SessionId::now_v7();
        let session = live_session(session_id);
        let previous = eval_row(EvaluationId::now_v7(), EvaluationStatus::Failed);
        let restarted = eval_row(EvaluationId::now_v7(), EvaluationStatus::Completed);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // authorize middleware
            .append_query_results([vec![session.clone()]])
            .append_query_results([vec![session]])
            .append_query_results([vec![user()]])
            // load_task + TriggerEvaluation permission
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
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role()]])
            // no evaluation currently active
            .append_query_results([Vec::<evaluation::Model>::new()])
            // previous evaluation, and its (absent) entry points
            .append_query_results([vec![previous]])
            .append_query_results([Vec::<entry_point::Model>::new()])
            // INSERT ... RETURNING the new evaluation
            .append_query_results([vec![restarted.clone()]])
            // no task-level flake input overrides to snapshot
            .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
            // task.last_evaluation update
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![task_row()]]);

        let server = make_test_server(db.into_connection());

        let res = server
            .post("/api/v1/tasks/test-project/test-task/evaluate")
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .json(&json!({ "mode": "restart_failed" }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(
            body["message"],
            restarted.id.to_string(),
            "expected the new evaluation id, got {}",
            body["message"]
        );
        Uuid::parse_str(body["message"].as_str().unwrap()).expect("message must be a UUID");
    });
}

/// Sequence up to the point the pinned-commit validation runs: authorize, then
/// `load_task` with the TriggerEvaluation permission.
fn authorized_db(session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    MockDatabase::new(DatabaseBackend::Postgres)
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
        .append_query_results([vec![admin_membership()]])
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

/// A pinned commit must be exact: a prefix would make the evaluation ambiguous,
/// and both validations run before any git work so a bad request costs nothing.
#[test]
fn rejects_a_commit_that_is_not_a_full_hash() {
    run(async {
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
    });
}

#[test]
fn rejects_an_unparsable_attr() {
    run(async {
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
    });
}
