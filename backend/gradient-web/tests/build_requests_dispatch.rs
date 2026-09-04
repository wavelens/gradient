/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/{session}/dispatch`
//! (issue #234, task 11). Covers the conflict/gone surfaces and the happy
//! path, which exercises the materialise, task, commit and evaluation steps
//! against a mock DB. The source NAR's cache-index row is the graph actor's
//! write, so it is not in this transaction.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use gradient_db::permissions::PermissionMask;
use gradient_entity::ids::*;
use gradient_entity::role;
use gradient_test_support::fixtures::{project_id, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

fn write_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "write".into(),
        permission: gradient_db::permissions::write_mask() as PermissionMask,
        ..Default::default()
    }
}

fn membership() -> gradient_entity::project_user::Model {
    gradient_entity::project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap()),
        project: project_id(),
        user: user_id(),
        role: BASE_ROLE_WRITE_ID,
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn upload_session(
    id: UploadSessionId,
    missing: Vec<String>,
    dispatched: bool,
    expired: bool,
) -> gradient_entity::upload_session::Model {
    let now = Utc::now().naive_utc();
    let expires_at = if expired {
        now - Duration::seconds(60)
    } else {
        now + Duration::hours(1)
    };
    gradient_entity::upload_session::Model {
        id,
        project: project_id(),
        manifest: json!([]),
        missing: serde_json::to_value(missing).unwrap(),
        created_at: now,
        expires_at,
        dispatched_at: if dispatched { Some(now) } else { None },
        ..Default::default()
    }
}

fn task_row(id: TaskId, managed: bool) -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        id,
        project: project_id(),
        name: "build-request".into(),
        active: true,
        display_name: "Build Requests".into(),
        description: "Server-managed task for `gradient build` submissions.".into(),
        repository: "build-request".into(),
        wildcard: "*".into(),
        last_check_at: chrono::NaiveDateTime::default(),
        created_by: user_id(),
        created_at: Utc::now().naive_utc(),
        managed,
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::SoftAbort,
        sign_cache: true,
        ..Default::default()
    }
}

fn commit_row() -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id: CommitId::now_v7(),
        message: "Build request".into(),
        hash: vec![0; 20],
        author: Some(user_id()),
        author_name: "Test User".into(),
    }
}

fn eval_row(task: TaskId, commit: CommitId) -> gradient_entity::evaluation::Model {
    let now = Utc::now().naive_utc();
    gradient_entity::evaluation::Model {
        id: EvaluationId::now_v7(),
        task: Some(task),
        repository: "/nix/store/abc-source".into(),
        commit,
        wildcard: "*".into(),
        status: gradient_entity::evaluation::EvaluationStatus::Queued,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
}

fn dispatch_url(session: UploadSessionId) -> String {
    format!("/api/v1/build-requests/{}/dispatch", session)
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn rejects_already_dispatched_session() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], true, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status(StatusCode::CONFLICT);
    });
}

#[test]
fn rejects_expired_session() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, true)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status(StatusCode::GONE);
    });
}

#[test]
fn rejects_session_with_missing_blobs() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let missing = vec!["a".repeat(64)];

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, missing, false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status(StatusCode::CONFLICT);
    });
}

#[test]
fn rejects_session_not_found() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([Vec::<gradient_entity::upload_session::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status(StatusCode::NOT_FOUND);
    });
}

#[test]
fn happy_path_creates_task_commit_and_evaluation() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let task_id = TaskId::now_v7();
        let task_model = task_row(task_id, true);
        let commit_model = commit_row();
        let eval_model = eval_row(task_id, commit_model.id);

        let updated = gradient_entity::upload_session::Model {
            dispatched_at: Some(Utc::now().naive_utc()),
            ..upload_session(upload, vec![], false, false)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]])
            // ensure_build_request_task → SELECT existing (None)
            .append_query_results([Vec::<gradient_entity::task::Model>::new()])
            // ensure_build_request_task → INSERT task (returns row)
            .append_query_results([vec![task_model.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // INSERT commit (returns row)
            .append_query_results([vec![commit_model.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // INSERT evaluation (returns row)
            .append_query_results([vec![eval_model.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // resolve_project_cache_name → project-cache link lookup (none → cache=null)
            .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
            // After tx commit: UPDATE upload_session
            .append_query_results([vec![updated]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(
            body["message"]["task"].as_str().unwrap(),
            task_id.to_string()
        );
        assert_eq!(
            body["message"]["commit"].as_str().unwrap(),
            commit_model.id.to_string()
        );
        assert_eq!(
            body["message"]["evaluation"].as_str().unwrap(),
            eval_model.id.to_string()
        );
        assert!(
            body["message"].as_object().unwrap().contains_key("cache"),
            "DispatchResponse must carry a `cache` field"
        );
        assert!(body["message"]["cache"].is_null());
    });
}

#[test]
fn rejects_local_input_override() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({
                "input_overrides": [{ "input_name": "nixpkgs", "url": "/home/u/np" }]
            }))
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
    });
}

#[test]
fn happy_path_reuses_existing_build_request_task() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let task_id = TaskId::now_v7();
        let task_model = task_row(task_id, true);
        let commit_model = commit_row();
        let eval_model = eval_row(task_id, commit_model.id);

        let updated = gradient_entity::upload_session::Model {
            dispatched_at: Some(Utc::now().naive_utc()),
            ..upload_session(upload, vec![], false, false)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]])
            // ensure_build_request_task → SELECT existing returns the row
            .append_query_results([vec![task_model.clone()]])
            .append_query_results([vec![commit_model.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![eval_model.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // resolve_project_cache_name → project-cache link lookup (none → cache=null)
            .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
            .append_query_results([vec![updated]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&dispatch_url(upload))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(
            body["message"]["task"].as_str().unwrap(),
            task_id.to_string()
        );
    });
}
