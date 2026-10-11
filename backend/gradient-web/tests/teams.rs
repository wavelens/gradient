/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{team, team_user};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db, ran};
use gradient_fixtures::rows::{team, team_member};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, make_test_server, signed_in, with_auth};
use gradient_types::{CreatePermission, SessionId};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn creating_a_team_respects_the_create_permission() {
    let (db, auth) = signed_in();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_team = CreatePermission::Superusers;
        })
        .server();

    let res = server
        .put("/api/v1/teams")
        .add_header("authorization", auth)
        .json(&json!({ "name": "platform", "display_name": "Platform" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn creating_a_team_makes_the_creator_its_admin() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([Vec::<team::Model>::new()])
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Admin)]])
        .append_exec_results([exec_ok(), exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .put("/api/v1/teams")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "platform", "display_name": "Platform" }))
        .await;

    res.assert_status_ok();
    drop(server);
    assert!(ran(conn, "INSERT INTO \"team_user\""));
}

#[tokio::test]
async fn only_a_superuser_maps_sso_groups_onto_a_team() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Admin)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/teams/platform")
        .add_header("authorization", auth)
        .json(&json!({ "oidc_group": "platform-team" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn only_a_superuser_grants_a_team_on_every_new_project() {
    for body in [
        json!({ "new_project_users": true, "new_project_role": "Admin" }),
        json!({ "new_project_workers": true }),
    ] {
        let (db, auth) = signed_in();
        let db = db
            .append_query_results([vec![team()]])
            .append_query_results([vec![team_member(TeamRole::Admin)]]);
        let server = make_test_server(db.into_connection());

        let res = server
            .patch("/api/v1/teams/platform")
            .add_header("authorization", auth)
            .json(&body)
            .await;

        res.assert_status(StatusCode::FORBIDDEN);
        let body: Value = res.json();
        assert_eq!(body["code"], "superuser_required");
    }
}

#[tokio::test]
async fn deleting_a_team_withdraws_its_workers_from_the_granted_projects() {
    let session_id = SessionId::now_v7();
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Admin)]])
        .append_query_results([Vec::<gradient_entity::team_worker::Model>::new()])
        .append_query_results([Vec::<gradient_entity::team_project::Model>::new()])
        .append_exec_results([exec_ok(), exec_ok()])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .delete("/api/v1/teams/platform")
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    drop(server);
    let looked_up_projects = all_statements(conn.into_transaction_log())
        .into_iter()
        .any(|s| s.sql.contains("FROM \"team_project\""));
    assert!(
        looked_up_projects,
        "deleting a team must find the projects whose jobs its workers lose"
    );
}

#[tokio::test]
async fn a_team_is_hidden_from_non_members() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![team()]])
        .append_query_results([Vec::<team_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/teams/platform")
        .add_header("authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_team_lists_recent_evaluations_of_its_granted_projects() {
    let session_id = SessionId::now_v7();
    let mut row = std::collections::BTreeMap::new();
    row.insert("id", sea_orm::Value::Uuid(Some(Uuid::now_v7())));
    row.insert("project", sea_orm::Value::String(Some("web".into())));
    row.insert("task", sea_orm::Value::String(Some("app".into())));
    row.insert("status", sea_orm::Value::Int(Some(3)));
    row.insert(
        "created_at",
        sea_orm::Value::ChronoDateTime(Some(test_date())),
    );
    let conn = with_auth(mock_db(), session_id)
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Member)]])
        .append_query_results([vec![row]])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .get("/api/v1/teams/platform/evaluations")
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"][0]["project"], "web");
    assert_eq!(body["message"][0]["task"], "app");
    drop(server);
    let only_shared_projects = all_statements(conn.into_transaction_log())
        .into_iter()
        .any(|s| s.sql.contains("\"includes_users\""));
    assert!(
        only_shared_projects,
        "members only see projects granted with users"
    );
}
