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
    project_user, role, team, team_project, team_project_request, team_user, team_worker,
};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_WRITE_ID};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult};
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

fn ran(db: DatabaseConnection, prefix: &str) -> bool {
    db.into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .any(|s| s.sql.starts_with(prefix))
}

fn admin_access() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::now_v7(),
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

fn write_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "Write".into(),
        permission: gradient_db::permissions::write_mask(),
        ..Default::default()
    }
}

fn grant(users: bool, workers: bool) -> team_project::Model {
    team_project::Model {
        id: TeamProjectId::now_v7(),
        team: team_id(),
        project: project_id(),
        role: users.then_some(BASE_ROLE_WRITE_ID),
        includes_users: users,
        includes_workers: workers,
        created_at: test_date(),
    }
}

#[tokio::test]
async fn a_project_admin_outside_the_team_asks_the_team_first() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![admin_access()]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![write_role()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_query_results([Vec::<team_user::Model>::new()])
        .append_query_results([vec![team_project_request::Model {
            team: team_id(),
            project: project_id(),
            role: Some(BASE_ROLE_WRITE_ID),
            includes_users: true,
            ..Default::default()
        }]])
        .append_exec_results([exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .post("/api/v1/projects/test-project/teams")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "team": "platform", "role": "Write", "users": true, "workers": false }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], "Request sent");
    drop(server);
    assert!(ran(conn, "INSERT INTO \"team_project_request\""));
}

#[tokio::test]
async fn a_team_admin_with_project_rights_grants_at_once() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![admin_access()]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![write_role()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_query_results([vec![membership(TeamRole::Admin)]])
        .append_query_results([vec![grant(true, false)]])
        .append_exec_results([exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .post("/api/v1/projects/test-project/teams")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "team": "platform", "role": "Write", "users": true, "workers": false }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], "Team granted");
    drop(server);
    assert!(ran(conn, "INSERT INTO \"team_project\""));
}

#[tokio::test]
async fn turning_on_a_teams_workers_needs_admin_in_the_team() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![admin_access()]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![grant(true, false)]])
        .append_query_results([Vec::<team_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/projects/test-project/teams/platform")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "workers": true }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn removing_a_grant_with_workers_withdraws_the_team_workers() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![grant(false, true)]])
        .append_query_results([vec![admin_access()]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([Vec::<team_worker::Model>::new()])
        .append_exec_results([exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .delete("/api/v1/projects/test-project/teams/platform")
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    drop(server);
    let looked_up_workers = conn
        .into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .any(|s| s.sql.contains("FROM \"team_worker\""));
    assert!(
        looked_up_workers,
        "removing workers must find the team's workers to withdraw"
    );
}
