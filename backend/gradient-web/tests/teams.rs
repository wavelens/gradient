/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{team, team_user};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db, ran};
use gradient_fixtures::rows::{superuser, team, team_member};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, make_test_server, signed_in, with_auth, with_auth_as};
use gradient_types::{CreatePermission, SessionId, TeamId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
    let body: Value = res.json();
    assert_eq!(
        body["message"]["id"],
        gradient_fixtures::ids::team_id().to_string()
    );
    assert_eq!(body["message"]["name"], "platform");
    assert!(
        body["message"]["url"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/team/platform")
    );
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
async fn granting_users_on_every_new_project_needs_a_role() {
    let session_id = SessionId::now_v7();
    let db = with_auth_as(mock_db(), session_id, superuser())
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Admin)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/teams/platform")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "new_project_users": true }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = res.json();
    assert_eq!(
        body["message"],
        "A role is required when the grant includes users."
    );
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

fn team_count(team: &team::Model, count: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([
        ("team", sea_orm::Value::Uuid(Some(team.id.into_inner()))),
        ("team_count", sea_orm::Value::BigInt(Some(count))),
    ])
}

#[tokio::test]
async fn all_teams_are_listed_with_counts_and_caller_admin_mark() {
    let joined = team();
    let foreign = team::Model {
        id: TeamId::now_v7(),
        name: "foreign".into(),
        display_name: "Foreign".into(),
        ..team()
    };
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![team_count(&joined, 2), team_count(&foreign, 1)]])
        .append_query_results([vec![team_count(&foreign, 3)]])
        .append_query_results([vec![team_member(TeamRole::Admin)]])
        .append_query_results([vec![foreign, joined]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/teams/all")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"],
        json!([
            { "name": "foreign", "display_name": "Foreign", "member_count": 1, "worker_count": 3, "caller_is_admin": false },
            { "name": "platform", "display_name": "Platform", "member_count": 2, "worker_count": 0, "caller_is_admin": true },
        ])
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
