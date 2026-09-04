/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the `GET /api/v1/tasks/{project}/{task}/evaluations`
//! search filters (#564).
//!
//! The `attr` filter matches an evaluation's *wildcard*, set when the row is
//! created, so it answers while an evaluation is still `Queued` - unlike entry
//! points, which only exist once derivations have resolved. That is the
//! behaviour `attr_filter_matches_queued_evaluation` pins.
//!
//! Every request (public project, so no membership check) consumes these
//! mocked reads before the handler's own: session by jti, session update, user,
//! project by name, task by project and name.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::{commit, evaluation, ids::*, project, task};
use gradient_test_support::fixtures::{commit_id, project_id, task_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

const COMMIT_HEX: &str = "1111111111111111111111111111111111111111";

fn public_project() -> project::Model {
    project::Model {
        id: project_id(),
        name: "test-project".into(),
        display_name: "Test Project".into(),
        public: true,
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

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

fn commit_row() -> commit::Model {
    commit::Model {
        id: commit_id(),
        message: "test commit".into(),
        hash: vec![0x11; 20],
        author: None,
        author_name: "Tester".into(),
    }
}

fn eval_row(id: EvaluationId, wildcard: &str, status: EvaluationStatus) -> evaluation::Model {
    evaluation::Model {
        id,
        task: Some(task_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: wildcard.into(),
        status,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn with_task(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![public_project()]])
        .append_query_results([vec![task_row()]])
}

/// The two grouped rollups `evaluations_to_summaries` runs after loading
/// commits. Empty is a valid result set for both.
fn with_summary_rollups(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<commit::Model>::new()])
        .append_query_results([Vec::<commit::Model>::new()])
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn base_db(session_id: SessionId) -> MockDatabase {
    with_task(with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
    ))
}

const URL: &str = "/api/v1/tasks/test-project/test-task/evaluations";

#[test]
fn rejects_commit_that_is_not_a_full_hash() {
    run(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server(base_db(session_id).into_connection());

        let res = server
            .get(&format!("{URL}?commit=1111"))
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(
            body["message"].as_str().unwrap().contains("40-character"),
            "unexpected message: {}",
            body["message"]
        );
    });
}

#[test]
fn rejects_unknown_status() {
    run(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server(base_db(session_id).into_connection());

        let res = server
            .get(&format!("{URL}?status=Exploded"))
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert!(
            body["message"].as_str().unwrap().contains("Exploded"),
            "unexpected message: {}",
            body["message"]
        );
    });
}

/// `attr` names one concrete attribute path; wildcard syntax there would make
/// "does this evaluation cover my attr" ambiguous in both directions.
#[test]
fn rejects_wildcard_syntax_in_attr() {
    run(async {
        let session_id = SessionId::now_v7();
        let server = make_test_server(base_db(session_id).into_connection());

        let res = server
            .get(&format!("{URL}?attr=packages.*.hello"))
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert!(
            body["message"].as_str().unwrap().contains("concrete"),
            "unexpected message: {}",
            body["message"]
        );
    });
}

/// A hash nothing was ever evaluated at is an empty result, not a 404 - the
/// caller is asking whether an evaluation exists.
#[test]
fn unknown_commit_returns_empty_list() {
    run(async {
        let session_id = SessionId::now_v7();
        let db = base_db(session_id).append_query_results([Vec::<commit::Model>::new()]);
        let server = make_test_server(db.into_connection());

        let res = server
            .get(&format!("{URL}?commit={COMMIT_HEX}"))
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"].as_array().unwrap().len(), 0);
    });
}

/// The core of #564: an evaluation that has not produced a single entry point
/// yet still answers "yes, this attr is in flight", because the match is on the
/// wildcard it was created with.
#[test]
fn attr_filter_matches_queued_evaluation() {
    run(async {
        let session_id = SessionId::now_v7();
        let queued = EvaluationId::now_v7();

        let db = base_db(session_id)
            .append_query_results([vec![commit_row()]])
            .append_query_results([vec![
                eval_row(queued, "packages.*.*", EvaluationStatus::Queued),
                eval_row(
                    EvaluationId::now_v7(),
                    "checks.*.*",
                    EvaluationStatus::Completed,
                ),
            ]])
            .append_query_results([vec![commit_row()]]);
        let server = make_test_server(with_summary_rollups(db).into_connection());

        let res = server
            .get(&format!(
                "{URL}?commit={COMMIT_HEX}&attr=packages.x86_64-linux.hello"
            ))
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let rows = body["message"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "checks.*.* must be filtered out: {body}");
        assert_eq!(rows[0]["id"], queued.to_string());
        assert_eq!(rows[0]["status"], "Queued");
        assert_eq!(rows[0]["wildcard"], "packages.*.*");
        assert_eq!(rows[0]["commit"], COMMIT_HEX);
    });
}
