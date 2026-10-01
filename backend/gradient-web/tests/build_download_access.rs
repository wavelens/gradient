/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /builds/{build}/download/{filename}` admits exactly the callers every
//! other build read admits (`BuildAccessContext::load`), or a download token for
//! the build's derivation.
//!
//! An admitted request with no build products answers `File not found`; a
//! refused one answers `Build not found`, so the message tells the two apart.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use chrono::{Duration, Utc};
use gradient_entity::ids::*;
use gradient_test_support::fixtures::{eval_at, project, project_id, task_id, user, user_id};
use gradient_test_support::web::{TEST_JWT_SECRET, live_session, make_test_server, make_token};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use uuid::Uuid;

fn build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb1))
}
fn other_build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb2))
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::from_u128(0xd1))
}
fn shared_build_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::from_u128(0xa1))
}
fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}
fn other_eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe2))
}
fn other_task_id() -> TaskId {
    TaskId::new(Uuid::from_u128(0x72))
}
fn other_project_id() -> ProjectId {
    ProjectId::new(Uuid::from_u128(0x92))
}

fn download_url(token: Option<&str>) -> String {
    let base = format!("/api/v1/builds/{}/download/image.iso", build_job_id());
    match token {
        Some(t) => format!("{base}?token={t}"),
        None => base,
    }
}

fn build_job(id: BuildJobId, evaluation: EvaluationId) -> gradient_entity::build_job::Model {
    gradient_entity::build_job::Model {
        id,
        evaluation,
        derivation: derivation_id(),
        derivation_build: shared_build_id(),
        ..Default::default()
    }
}

fn shared_build() -> gradient_entity::derivation_build::Model {
    gradient_entity::derivation_build::Model {
        id: shared_build_id(),
        derivation: derivation_id(),
        ..Default::default()
    }
}

fn task(id: TaskId, project: ProjectId) -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        id,
        project,
        ..Default::default()
    }
}

/// The private project owning [`build_job_id`]: build_job, shared build, evaluation,
/// task, project, in `BuildAccessContext::load_unguarded` order.
fn with_private_build(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![build_job(build_job_id(), eval_id())]])
        .append_query_results([vec![shared_build()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task(task_id(), project_id())]])
        .append_query_results([vec![project()]])
}

fn with_session(db: MockDatabase, session: SessionId) -> MockDatabase {
    db.append_query_results([vec![live_session(session)]])
        .append_query_results([vec![live_session(session)]])
        .append_query_results([vec![user()]])
}

fn no_outputs(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
}

fn download_token(derivation: DerivationId) -> String {
    let now = Utc::now();
    let claims = serde_json::json!({
        "iat": now.timestamp(),
        "exp": (now + Duration::hours(1)).timestamp(),
        "derivation": derivation,
        "evaluation": eval_id(),
    });
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

fn message(res: &axum_test::TestResponse) -> String {
    let body: Value = res.json();
    body["message"].as_str().unwrap_or_default().to_owned()
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// A member of another project that built the same derivation reads its log,
/// graph and product list; the file itself must not be the one read refused.
#[test]
fn a_member_of_a_project_that_built_the_derivation_downloads_without_a_token() {
    run(async {
        let session = SessionId::now_v7();
        let db = with_private_build(with_session(
            MockDatabase::new(DatabaseBackend::Postgres),
            session,
        ))
        .append_query_results([Vec::<gradient_entity::project_user::Model>::new()])
        .append_query_results([vec![build_job(other_build_job_id(), other_eval_id())]])
        .append_query_results([vec![gradient_entity::evaluation::Model {
            task: Some(other_task_id()),
            ..eval_at(other_eval_id(), 0)
        }]])
        .append_query_results([vec![task(other_task_id(), other_project_id())]])
        .append_query_results([vec![gradient_entity::project_user::Model {
            id: ProjectUserId::new(Uuid::from_u128(0x51)),
            project: other_project_id(),
            user: user_id(),
            role: gradient_types::consts::BASE_ROLE_VIEW_ID,
        }]]);
        let server = make_test_server(no_outputs(db).into_connection());

        let res = server
            .get(&download_url(None))
            .add_header("authorization", format!("Bearer {}", make_token(session)))
            .await;

        res.assert_status_not_found();
        assert_eq!(message(&res), "File not found");
    });
}

#[test]
fn an_anonymous_caller_without_a_token_is_refused_on_a_private_project() {
    run(async {
        let db = with_private_build(MockDatabase::new(DatabaseBackend::Postgres));
        let server = make_test_server(db.into_connection());

        let res = server.get(&download_url(None)).await;

        res.assert_status_not_found();
        assert_eq!(message(&res), "Build not found");
    });
}

#[test]
fn a_token_for_the_derivation_admits_an_anonymous_caller() {
    run(async {
        let db = no_outputs(with_private_build(MockDatabase::new(
            DatabaseBackend::Postgres,
        )));
        let server = make_test_server(db.into_connection());

        let res = server
            .get(&download_url(Some(&download_token(derivation_id()))))
            .await;

        res.assert_status_not_found();
        assert_eq!(message(&res), "File not found");
    });
}

#[test]
fn a_token_for_another_derivation_is_refused() {
    run(async {
        let db = with_private_build(MockDatabase::new(DatabaseBackend::Postgres));
        let server = make_test_server(db.into_connection());
        let other = DerivationId::new(Uuid::from_u128(0xd9));

        let res = server
            .get(&download_url(Some(&download_token(other))))
            .await;

        res.assert_status_not_found();
        assert_eq!(message(&res), "Build not found");
    });
}
