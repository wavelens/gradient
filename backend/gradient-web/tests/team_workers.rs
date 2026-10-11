/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{
    project_user, team, team_project, team_user, team_worker, worker_registration,
};
use gradient_fixtures::ids::{project_id, team_id, user_id};
use gradient_fixtures::mock::exec_ok;
use gradient_fixtures::rows::project;
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in};
use serde_json::{Value, json};

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
        ..Default::default()
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
async fn an_admin_of_a_state_managed_team_registers_a_worker_and_sees_its_token_once() {
    let (db, auth) = signed_in();
    let managed_team = team::Model {
        managed: true,
        ..team_row()
    };
    let db = db
        .append_query_results([vec![managed_team]])
        .append_query_results([vec![membership(TeamRole::Admin)]])
        .append_query_results([Vec::<worker_registration::Model>::new()])
        .append_query_results([Vec::<team_worker::Model>::new()])
        .append_query_results([vec![team_worker_row()]])
        .append_query_results([Vec::<team_project::Model>::new()])
        .append_exec_results([exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/teams/platform/workers")
        .add_header("authorization", auth)
        .json(&json!({ "worker_id": worker_id(), "display_name": "Builder" }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["token"].as_str().map(str::len), Some(64));
}

#[tokio::test]
async fn a_project_cannot_change_a_team_worker() {
    let (db, auth) = signed_in();
    let db = db
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
        .add_header("authorization", auth)
        .json(&json!({ "active": false }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}
