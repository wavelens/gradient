/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::user;
use gradient_test_support::web::make_test_server;
use sea_orm::{MockDatabase, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use uuid::Uuid;

const TOKEN: &str = "Bearer GRJ_0123456789012345678901234567890123456789012345678901234567890123";

fn resolved_token() -> BTreeMap<&'static str, Value> {
    BTreeMap::from([
        ("job", Value::from(Uuid::from_u128(0x7a))),
        ("attempt", Value::from(Uuid::from_u128(0xa1))),
        ("project", Value::from(project_id().into_inner())),
        ("task", Value::from(task_id().into_inner())),
        ("project_name", Value::from("infra")),
        ("task_name", Value::from("master")),
        ("acting_user", Value::from(user_id().into_inner())),
    ])
}

fn with_job_token(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![resolved_token()]])
}

fn assert_refused_by_scope(status: StatusCode, body: &Json) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["message"],
        "job tokens only reach their own project and task"
    );
}

#[tokio::test]
async fn a_job_token_reads_its_own_task() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/tasks/infra/master")
        .add_header("Authorization", TOKEN)
        .await;

    assert_ne!(res.status_code(), StatusCode::UNAUTHORIZED);
    assert_ne!(res.status_code(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_job_token_is_refused_on_account_routes() {
    let server = make_test_server(with_job_token(mock_db()).into_connection());

    let res = server
        .get("/api/v1/user")
        .add_header("Authorization", TOKEN)
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn a_job_token_on_an_optional_route_outside_its_scope_is_refused() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/projects/other")
        .add_header("Authorization", TOKEN)
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn a_revoked_job_token_is_unauthorized() {
    let db = mock_db().append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/tasks/infra/master/evaluate")
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_unauthorized();
    assert_eq!(res.json::<Json>()["message"], "Unable to decode token");
}
