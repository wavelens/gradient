/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{role, team_project, team_project_request, team_user, team_worker};
use gradient_fixtures::ids::{project_id, team_id, user_id};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db, ran};
use gradient_fixtures::rows::{admin_role, project, project_member, team, team_member, write_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::{bearer, make_test_server, signed_in, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_WRITE_ID};
use gradient_types::permissions::{Permission, mask_from};
use serde_json::{Value, json};

fn workers_manager_role() -> role::Model {
    role::Model {
        id: BASE_ROLE_WRITE_ID,
        name: "Workers".into(),
        permission: mask_from(&[Permission::ManageWorkers]),
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
        managed: false,
        created_at: test_date(),
    }
}

#[tokio::test]
async fn a_project_admin_outside_the_team_asks_the_team_first() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team()]])
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
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team()]])
        .append_query_results([vec![write_role()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_query_results([vec![team_member(TeamRole::Admin)]])
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
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![team()]])
        .append_query_results([vec![grant(true, false)]])
        .append_query_results([Vec::<team_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/projects/test-project/teams/platform")
        .add_header("authorization", auth)
        .json(&json!({ "workers": true }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_project_side_withdraws_a_pending_request() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![team()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_query_results([vec![team_project_request::Model {
            id: TeamProjectRequestId::now_v7(),
            team: team_id(),
            project: project_id(),
            role: Some(BASE_ROLE_WRITE_ID),
            includes_users: true,
            includes_workers: false,
            requested_by: Some(user_id()),
            created_at: test_date(),
        }]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_exec_results([exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .delete("/api/v1/projects/test-project/teams/platform")
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], "Request withdrawn");
    drop(server);
    assert!(ran(conn, "DELETE FROM \"team_project_request\""));
}

#[tokio::test]
async fn removing_a_grant_with_workers_withdraws_the_team_workers() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![team()]])
        .append_query_results([vec![grant(false, true)]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
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
    let looked_up_workers = all_statements(conn.into_transaction_log())
        .into_iter()
        .any(|s| s.sql.contains("FROM \"team_worker\""));
    assert!(
        looked_up_workers,
        "removing workers must find the team's workers to withdraw"
    );
}

#[tokio::test]
async fn managing_workers_is_enough_to_remove_a_workers_only_grant() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![team()]])
        .append_query_results([vec![grant(false, true)]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![workers_manager_role()]])
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
    assert!(ran(conn, "DELETE FROM \"team_project\""));
}

#[tokio::test]
async fn managing_workers_is_not_enough_to_remove_a_grant_with_users() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![team()]])
        .append_query_results([vec![grant(true, true)]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![workers_manager_role()]])
        .append_query_results([Vec::<team_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .delete("/api/v1/projects/test-project/teams/platform")
        .add_header("authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}
