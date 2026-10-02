/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{ids::*, project_user};
use gradient_test_support::fixtures::{project, project_id, user, user_id};
use gradient_test_support::web::{
    live_session, make_test_server, make_test_server_configured, make_token,
};
use gradient_types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};

const URL: &str = "/api/v1/gradient-ci/connections";
const TOKEN: &str = "gci1_0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b_s3cret";

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
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
async fn a_base_connection_needs_a_superuser() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id);
    let server = make_test_server(db.into_connection());

    let res = server
        .post(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "scope": "base", "token": TOKEN }))
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
