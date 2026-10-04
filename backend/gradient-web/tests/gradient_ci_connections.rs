/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::team_user::TeamRole;
use gradient_entity::{ids::*, project_user, team, team_user, team_worker, worker_registration};
use gradient_test_support::fixtures::{project, project_id, user, user_id};
use gradient_test_support::web::{
    live_session, make_test_server, make_test_server_configured, make_test_server_with, make_token,
};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, DbErr, MockDatabase, RuntimeErr, sqlx};
use serde_json::{Value, json};

const URL: &str = "/api/v1/gradient-ci/connections";
const WORKER_ID: &str = "0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b";
const TOKEN: &str = "gci1_0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b_s3cret";

#[derive(Debug)]
struct UniqueViolation;

impl std::fmt::Display for UniqueViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("duplicate key value violates unique constraint")
    }
}

impl std::error::Error for UniqueViolation {}

impl sqlx::error::DatabaseError for UniqueViolation {
    fn message(&self) -> &str {
        "duplicate key value violates unique constraint"
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::UniqueViolation
    }
}

fn unique_violation() -> DbErr {
    DbErr::Query(RuntimeErr::SqlxError(Arc::new(sqlx::Error::Database(
        Box::new(UniqueViolation),
    ))))
}

fn temp_crypt_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", uuid::Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    path.to_string_lossy().into_owned()
}

fn as_member(session_id: SessionId) -> MockDatabase {
    with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![member()]])
}

async fn connect_project(
    server: &axum_test::TestServer,
    session_id: SessionId,
) -> axum_test::TestResponse {
    server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "project", "project": "test-project", "token": TOKEN }))
        .await
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn team_row() -> team::Model {
    team::Model {
        id: TeamId::new(
            uuid::Uuid::parse_str("7e000000-0000-0000-0000-000000000001").expect("uuid"),
        ),
        name: "platform".into(),
        display_name: "Platform".into(),
        created_by: Some(user_id()),
        created_at: gradient_test_support::fixtures::test_date(),
        ..Default::default()
    }
}

fn member() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::now_v7(),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_VIEW_ID,
    }
}

#[tokio::test]
async fn connecting_is_refused_while_gradient_ci_is_turned_off() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id);
    let server =
        make_test_server_configured(db.into_connection(), |cli| cli.gradient_ci.enable = false);

    let res = server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "project", "project": "test-project", "token": TOKEN }))
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn a_team_connection_needs_a_team_admin() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![team_user::Model {
            id: TeamUserId::now_v7(),
            team: team_row().id,
            user: user_id(),
            role: TeamRole::Member,
            ..Default::default()
        }]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "team", "team": "platform", "token": TOKEN }))
        .await;

    res.assert_status_forbidden();
}

#[tokio::test]
async fn a_project_connection_is_hidden_from_non_members() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "project", "project": "test-project", "token": TOKEN }))
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn a_misplaced_paste_is_a_bad_request_naming_the_prefix() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![member()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "project", "project": "test-project", "token": "GRADabc" }))
        .await;

    res.assert_status_bad_request();
    let body: Value = res.json();
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("gci1_")
    );
}

#[tokio::test]
async fn a_worker_connected_by_another_project_cannot_be_claimed() {
    let session_id = SessionId::now_v7();
    let db = as_member(session_id)
        .append_query_results([vec![worker_registration::Model {
            peer_id: ProjectId::now_v7(),
            worker_id: WORKER_ID.into(),
            gradient_ci: true,
            ..Default::default()
        }]])
        .append_query_results([Vec::<team_worker::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = connect_project(&server, session_id).await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_concurrent_second_connection_is_a_conflict() {
    let session_id = SessionId::now_v7();
    let db = as_member(session_id)
        .append_query_results([Vec::<worker_registration::Model>::new()])
        .append_query_results([Vec::<team_worker::Model>::new()])
        .append_query_errors([unique_violation()]);
    let server = make_test_server_with(db.into_connection(), Some(temp_crypt_file()));

    let res = connect_project(&server, session_id).await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
}
