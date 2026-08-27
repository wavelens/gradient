/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/url` (#564).
//!
//! The no-upload build request: the source is already published at a URL, so a
//! deployment tool posts the URL and a revision instead of shipping a source
//! tree. Every test here pins an explicit `rev`, which is the branch that does
//! no network work - resolving a `ref` needs a real remote.
//!
//! Query sequence: session, session update, user (authorize), project by name,
//! membership, role (TriggerEvaluation), then the queueing transaction.

use gradient_db::permissions::PermissionMask;
use gradient_entity::{ids::*, project, project_user, role, task};
use gradient_test_support::fixtures::{project_id, task_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

const URL: &str = "/api/v1/build-requests/url";
const REV: &str = "9c1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3";
const REPO: &str = "https://example.com/org/repo.git";

fn write_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "write".into(),
        permission: gradient_db::permissions::write_mask() as PermissionMask,
        ..Default::default()
    }
}

fn membership() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap()),
        project: project_id(),
        user: user_id(),
        role: BASE_ROLE_WRITE_ID,
    }
}

fn project_row() -> project::Model {
    project::Model {
        id: project_id(),
        name: "test-project".into(),
        display_name: "Test Project".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn reserved_task() -> task::Model {
    task::Model {
        id: task_id(),
        project: project_id(),
        name: "build-request".into(),
        active: true,
        display_name: "Build Requests".into(),
        repository: "build-request".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        managed: true,
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::All,
        ..Default::default()
    }
}

fn base_db(session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![membership()]])
        .append_query_results([vec![write_role()]])
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

async fn post(db: MockDatabase, session_id: SessionId, body: Value) -> axum_test::TestResponse {
    make_test_server(db.into_connection())
        .post(URL)
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&body)
        .await
}

#[test]
fn queues_an_evaluation_against_the_remote_url() {
    run(async {
        let session_id = SessionId::now_v7();
        let commit = CommitId::now_v7();
        let evaluation = EvaluationId::now_v7();

        let db = base_db(session_id)
            // reserved build-request task already exists
            .append_query_results([vec![reserved_task()]])
            // INSERT commit, INSERT evaluation
            .append_query_results([vec![gradient_entity::commit::Model {
                id: commit,
                message: format!("Build request {REPO}@{REV}"),
                hash: vec![0x9c; 20],
                author: Some(user_id()),
                author_name: user().name,
            }]])
            .append_query_results([vec![gradient_entity::evaluation::Model {
                id: evaluation,
                task: Some(task_id()),
                repository: format!("git+{REPO}?rev={REV}"),
                commit,
                wildcard: "packages.x86_64-linux.hello".into(),
                concurrent: true,
                created_at: test_date(),
                updated_at: test_date(),
                ..Default::default()
            }]])
            // no cache linked to the project
            .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let res = post(
            db,
            session_id,
            json!({
                "project": "test-project",
                "url": REPO,
                "rev": REV,
                "target": "packages.x86_64-linux.hello",
            }),
        )
        .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["evaluation"], evaluation.to_string());
        assert_eq!(body["message"]["task"], task_id().to_string());
        assert_eq!(body["message"]["commit"], commit.to_string());
    });
}

#[test]
fn rejects_ref_and_rev_together() {
    run(async {
        let session_id = SessionId::now_v7();
        let res = post(
            base_db(session_id),
            session_id,
            json!({"project": "test-project", "url": REPO, "rev": REV, "ref": "main"}),
        )
        .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert!(
            body["message"].as_str().unwrap().contains("at most one"),
            "unexpected message: {}",
            body["message"]
        );
    });
}

#[test]
fn rejects_a_rev_that_is_not_a_full_hash() {
    run(async {
        let session_id = SessionId::now_v7();
        let res = post(
            base_db(session_id),
            session_id,
            json!({"project": "test-project", "url": REPO, "rev": "9c1a2b3"}),
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
fn rejects_an_empty_url() {
    run(async {
        let session_id = SessionId::now_v7();
        let res = post(
            base_db(session_id),
            session_id,
            json!({"project": "test-project", "url": "   ", "rev": REV}),
        )
        .await;

        res.assert_status_bad_request();
    });
}

/// `repository_url_to_nix` refuses local paths, so a build request cannot make
/// the server read a flake off its own disk.
#[test]
fn rejects_a_local_file_url() {
    run(async {
        let session_id = SessionId::now_v7();
        let res = post(
            base_db(session_id),
            session_id,
            json!({"project": "test-project", "url": "file:///etc", "rev": REV}),
        )
        .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert!(
            body["message"].as_str().unwrap().contains("repository URL"),
            "unexpected message: {}",
            body["message"]
        );
    });
}

/// Same defence in depth the upload dispatch applies: an override must name a
/// remote flake ref, never a local path.
#[test]
fn rejects_a_local_input_override() {
    run(async {
        let session_id = SessionId::now_v7();
        let res = post(
            base_db(session_id),
            session_id,
            json!({
                "project": "test-project",
                "url": REPO,
                "rev": REV,
                "input_overrides": [{"input_name": "nixpkgs", "url": "/home/me/nixpkgs"}],
            }),
        )
        .await;

        res.assert_status_bad_request();
    });
}
