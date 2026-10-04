/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{
    project_user, team, team_project, team_user, team_worker, worker_registration,
};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use uuid::Uuid;

fn team_id() -> TeamId {
    TeamId::new(Uuid::parse_str("7e000000-0000-0000-0000-000000000001").unwrap())
}

fn team_row() -> team::Model {
    team::Model {
        id: team_id(),
        name: "platform".into(),
        display_name: "Platform".into(),
        created_by: Some(user_id()),
        created_at: test_date(),
        ..Default::default()
    }
}

fn membership(role: TeamRole) -> team_user::Model {
    team_user::Model {
        id: TeamUserId::now_v7(),
        team: team_id(),
        user: user_id(),
        role,
        via_group: false,
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

fn exec_ok() -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    }
}

fn worker_id() -> &'static str {
    "7e000000-0000-0000-0000-0000000000f1"
}

fn team_worker_row() -> team_worker::Model {
    team_worker::Model {
        id: TeamWorkerId::now_v7(),
        team: team_id(),
        worker_id: worker_id().into(),
        token_hash: "hash".into(),
        display_name: "Builder".into(),
        active: true,
        enable_fetch: true,
        enable_eval: true,
        enable_build: true,
        created_at: test_date(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_team_admin_registers_a_worker_and_sees_its_token_once() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![membership(TeamRole::Admin)]])
        .append_query_results([Vec::<worker_registration::Model>::new()])
        .append_query_results([Vec::<team_worker::Model>::new()])
        .append_query_results([vec![team_worker_row()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_exec_results([exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/teams/platform/workers")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "worker_id": worker_id(), "display_name": "Builder" }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["token"].as_str().map(str::len), Some(64));
}

#[tokio::test]
async fn a_project_cannot_change_a_team_worker() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_user::Model {
            id: ProjectUserId::now_v7(),
            project: project_id(),
            user: user_id(),
            role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        }]])
        .append_query_results([vec![team_worker_row()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch(&format!(
            "/api/v1/projects/test-project/workers/{}",
            worker_id()
        ))
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "active": false }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}
