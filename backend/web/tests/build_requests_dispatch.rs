/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `POST /api/v1/build-requests/{session}/dispatch`
//! (issue #234, task 11). Covers the conflict/gone surfaces and the happy
//! path which exercises the full materialise → cached_path → project →
//! commit → evaluation pipeline against a mock DB.

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use entity::ids::*;
use entity::role;
use gradient_core::permissions::PermissionMask;
use gradient_core::types::SessionId;
use gradient_core::types::consts::BASE_ROLE_WRITE_ID;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use test_support::fixtures::{org_id, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

fn write_role_row() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "write".into(),
        organization: None,
        permission: gradient_core::permissions::write_mask() as PermissionMask,
        managed: false,
    }
}

fn membership() -> entity::organization_user::Model {
    entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap(),
        ),
        organization: org_id(),
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
) -> entity::upload_session::Model {
    let now = Utc::now().naive_utc();
    let expires_at = if expired {
        now - Duration::seconds(60)
    } else {
        now + Duration::hours(1)
    };
    entity::upload_session::Model {
        id,
        organization: org_id(),
        manifest: json!([]),
        missing: serde_json::to_value(missing).unwrap(),
        total_size: 0,
        created_at: now,
        expires_at,
        dispatched_at: if dispatched { Some(now) } else { None },
    }
}

fn project_row(id: ProjectId, managed: bool) -> entity::project::Model {
    entity::project::Model {
        id,
        organization: org_id(),
        name: "build-request".into(),
        active: true,
        display_name: "Build Requests".into(),
        description: "Server-managed project for `gradient build` submissions.".into(),
        repository: "build-request".into(),
        wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: chrono::NaiveDateTime::default(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: Utc::now().naive_utc(),
        managed,
        keep_evaluations: 30,
        concurrency: 1,
        sign_cache: true,
    }
}

fn cached_path_row(hash: &str) -> entity::cached_path::Model {
    entity::cached_path::Model {
        id: CachedPathId::now_v7(),
        store_path: format!("/nix/store/{}-source", hash),
        hash: hash.into(),
        package: "source".into(),
        file_hash: Some(format!("sha256:{}", hash)),
        file_size: Some(0),
        nar_size: Some(0),
        nar_hash: Some(format!("sha256:{}", hash)),
        references: None,
        ca: None,
        deriver: None,
        created_at: Utc::now().naive_utc(),
    }
}

fn commit_row() -> entity::commit::Model {
    entity::commit::Model {
        id: CommitId::now_v7(),
        message: "Build request".into(),
        hash: vec![0; 20],
        author: Some(user_id()),
        author_name: "Test User".into(),
    }
}

fn eval_row(project: ProjectId, commit: CommitId) -> entity::evaluation::Model {
    let now = Utc::now().naive_utc();
    entity::evaluation::Model {
        id: EvaluationId::now_v7(),
        project: Some(project),
        repository: "/nix/store/abc-source".into(),
        commit,
        wildcard: "*".into(),
        status: entity::evaluation::EvaluationStatus::Queued,
        previous: None,
        next: None,
        created_at: now,
        updated_at: now,
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
        source_comment: None,
        fetch_started_at: None,
        eval_flake_started_at: None,
        eval_drv_started_at: None,
        building_started_at: None,
        finished_at: None,
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
            .append_query_results([Vec::<entity::upload_session::Model>::new()]);

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
fn happy_path_creates_project_commit_and_evaluation() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let project_id = ProjectId::now_v7();
        let project_model = project_row(project_id, true);
        let commit_model = commit_row();
        let eval_model = eval_row(project_id, commit_model.id);
        let cp_row = cached_path_row("00000000000000000000000000000000");

        let updated = entity::upload_session::Model {
            dispatched_at: Some(Utc::now().naive_utc()),
            ..upload_session(upload, vec![], false, false)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]])
            // ensure_cached_path → SELECT (None)
            .append_query_results([Vec::<entity::cached_path::Model>::new()])
            // ensure_cached_path → INSERT (returns row)
            .append_query_results([vec![cp_row]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // queue_signature_placeholders → list org caches (empty, early return)
            .append_query_results([Vec::<entity::organization_cache::Model>::new()])
            // ensure_build_request_project → SELECT existing (None)
            .append_query_results([Vec::<entity::project::Model>::new()])
            // ensure_build_request_project → INSERT project (returns row)
            .append_query_results([vec![project_model.clone()]])
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
            body["message"]["project"].as_str().unwrap(),
            project_id.to_string()
        );
        assert_eq!(
            body["message"]["commit"].as_str().unwrap(),
            commit_model.id.to_string()
        );
        assert_eq!(
            body["message"]["evaluation"].as_str().unwrap(),
            eval_model.id.to_string()
        );
    });
}

#[test]
fn happy_path_reuses_existing_build_request_project() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let upload = UploadSessionId::now_v7();

        let project_id = ProjectId::now_v7();
        let project_model = project_row(project_id, true);
        let commit_model = commit_row();
        let eval_model = eval_row(project_id, commit_model.id);
        let cp_row = cached_path_row("00000000000000000000000000000000");

        let updated = entity::upload_session::Model {
            dispatched_at: Some(Utc::now().naive_utc()),
            ..upload_session(upload, vec![], false, false)
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![upload_session(upload, vec![], false, false)]])
            .append_query_results([vec![membership()]])
            .append_query_results([vec![write_role_row()]])
            // ensure_cached_path → SELECT existing returns the row
            .append_query_results([vec![cp_row]])
            // queue_signature_placeholders → org caches (empty)
            .append_query_results([Vec::<entity::organization_cache::Model>::new()])
            // ensure_build_request_project → SELECT existing returns the row
            .append_query_results([vec![project_model.clone()]])
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
            body["message"]["project"].as_str().unwrap(),
            project_id.to_string()
        );
    });
}
