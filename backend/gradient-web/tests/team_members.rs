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
use gradient_entity::team_user::{TeamMemberSource, TeamRole};
use gradient_entity::{team, team_invitation, team_user};
use gradient_test_support::fixtures::{test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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

fn managed_team_row() -> team::Model {
    team::Model {
        managed: true,
        ..team_row()
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

fn other_user_id() -> UserId {
    UserId::new(Uuid::parse_str("7e000000-0000-0000-0000-000000000002").unwrap())
}

fn other_user() -> gradient_entity::user::Model {
    gradient_entity::user::Model {
        id: other_user_id(),
        username: "otheruser".into(),
        name: "Other User".into(),
        email: "other@example.com".into(),
        email_verified: true,
        created_at: test_date(),
        last_login_at: test_date(),
        ..Default::default()
    }
}

fn member_row(user: UserId, role: TeamRole) -> team_user::Model {
    team_user::Model {
        id: TeamUserId::now_v7(),
        team: team_id(),
        user,
        role,
        ..Default::default()
    }
}

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

fn invitation() -> team_invitation::Model {
    team_invitation::Model {
        id: TeamInvitationId::now_v7(),
        team: team_id(),
        user: user_id(),
        role: TeamRole::Member,
        invited_by: other_user_id(),
        token: "team-tok".into(),
        created_at: test_date(),
        expires_at: chrono::Utc::now().naive_utc() + chrono::Duration::days(1),
    }
}

fn state_member() -> team_user::Model {
    team_user::Model {
        source: TeamMemberSource::State,
        ..member_row(other_user_id(), TeamRole::Member)
    }
}

fn with_state_member(session_id: SessionId) -> MockDatabase {
    with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![managed_team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![other_user()]])
        .append_query_results([vec![state_member()]])
}

#[tokio::test]
async fn an_admin_invites_a_user_into_a_state_managed_team() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![managed_team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![other_user()]])
        .append_query_results([Vec::<team_user::Model>::new()])
        .append_query_results([Vec::<team_invitation::Model>::new()])
        .append_query_results([vec![team_invitation::Model {
            user: other_user_id(),
            ..invitation()
        }]])
        .append_exec_results([exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "otheruser", "role": "member" }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], "Invitation sent");
}

#[tokio::test]
async fn a_member_declared_by_the_state_cannot_change_role() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(with_state_member(session_id).into_connection());

    let res = server
        .patch("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "otheruser", "role": "admin" }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_member_declared_by_the_state_cannot_be_removed() {
    let session_id = SessionId::now_v7();
    let server = make_test_server(with_state_member(session_id).into_connection());

    let res = server
        .delete("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "otheruser" }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_member_cannot_invite() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Member)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "otheruser", "role": "member" }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_last_admin_cannot_be_removed() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![count_row(1)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .delete("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "testuser" }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_last_admin_cannot_be_demoted() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![count_row(1)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "testuser", "role": "member" }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_group_member_made_admin_stays_past_the_group_sync() {
    let session_id = SessionId::now_v7();
    let group_member = team_user::Model {
        source: TeamMemberSource::Group,
        ..member_row(other_user_id(), TeamRole::Member)
    };
    let conn = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![team_row()]])
        .append_query_results([vec![member_row(user_id(), TeamRole::Admin)]])
        .append_query_results([vec![other_user()]])
        .append_query_results([vec![group_member.clone()]])
        .append_query_results([vec![team_user::Model {
            role: TeamRole::Admin,
            source: TeamMemberSource::Api,
            ..group_member
        }]])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .patch("/api/v1/teams/platform/members")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "user": "otheruser", "role": "admin" }))
        .await;

    res.assert_status_ok();
    drop(server);
    let clears_group_flag = conn
        .into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .any(|s| s.sql.starts_with("UPDATE \"team_user\"") && s.sql.contains("\"source\""));
    assert!(
        clears_group_flag,
        "a new Admin must no longer count as added by a group"
    );
}

#[tokio::test]
async fn accepting_a_team_invite_adds_the_membership() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([Vec::<gradient_entity::project_invitation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_invitation::Model>::new()])
        .append_query_results([vec![invitation()]])
        .append_query_results([Vec::<team_user::Model>::new()])
        .append_query_results([vec![member_row(user_id(), TeamRole::Member)]])
        .append_exec_results([exec_ok(), exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/user/invites/accept")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "token": "team-tok" }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"], "Invitation accepted");
}
