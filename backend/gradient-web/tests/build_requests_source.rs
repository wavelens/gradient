/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/source` (#422). The
//! `nix`-feature CLI uploads a pre-packed source NAR; the server computes the
//! store path and queues a build-request evaluation.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::StatusCode;
use axum_test::multipart::{MultipartForm, Part};
use chrono::Utc;
use gradient_db::permissions::PermissionMask;
use gradient_entity::ids::*;
use gradient_entity::role;
use gradient_test_support::fixtures::{project_id, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::consts::BASE_ROLE_WRITE_ID;
use gradient_types::{ConcurrencyPolicy, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
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

fn with_project_access(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![gradient_test_support::fixtures::project()]])
        .append_query_results([vec![membership()]])
        .append_query_results([vec![write_role_row()]])
}

fn task_row(id: TaskId) -> gradient_entity::task::Model {
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
        managed: true,
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

fn source_url() -> String {
    "/api/v1/build-requests/source?project=test-project".to_string()
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn source_upload_creates_queued_eval() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let task_id = TaskId::now_v7();
        let task_model = task_row(task_id);
        let commit_model = commit_row();
        let eval_model = eval_row(task_id, commit_model.id);

        let db = with_project_access(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // ensure_build_request_task → SELECT (None) then INSERT
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .append_query_results([vec![task_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // INSERT commit
        .append_query_results([vec![commit_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // INSERT evaluation
        .append_query_results([vec![eval_model.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // resolve_project_cache_name → project-cache link lookup (none → cache=null)
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()]);

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new()
            .add_part("nar", Part::bytes(b"a pretend source nar".to_vec()))
            .add_text("target", "packages.x86_64-linux.hello")
            .add_text("system", "x86_64-linux");

        let res = server
            .post(&source_url())
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(
            body["message"]["evaluation"].as_str().unwrap(),
            eval_model.id.to_string()
        );
        assert!(body["message"]["cache"].is_null());
    });
}

#[test]
fn source_upload_missing_nar_is_400() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_access(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());

        let form = MultipartForm::new().add_text("target", "x");

        let res = server
            .post(&source_url())
            .add_header("authorization", format!("Bearer {}", token))
            .multipart(form)
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
    });
}
