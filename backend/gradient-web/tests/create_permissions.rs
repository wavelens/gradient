/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::cache;
use gradient_entity::ids::*;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{project_named, superuser, user};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_token, signed_in, signed_in_as, with_auth_as};
use gradient_types::{CreatePermission, SessionId};
use serde_json::{Value, json};

fn cache_row(name: &str) -> cache::Model {
    cache::Model {
        id: CacheId::now_v7(),
        name: name.to_string(),
        display_name: format!("{} display", name),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn project_body() -> Value {
    json!({ "name": "acme", "display_name": "Acme", "description": "", "public": false })
}

fn cache_body() -> Value {
    json!({
        "name": "acme", "display_name": "Acme", "description": "",
        "priority": 40, "local_priority": 40, "public": false,
    })
}

#[tokio::test]
async fn create_project_superusers_rejects_regular_user() {
    let (db, auth) = signed_in();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_project = CreatePermission::Superusers;
        })
        .server();

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", auth)
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn create_project_none_rejects_superuser() {
    let (db, auth) = signed_in_as(superuser());
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_project = CreatePermission::None;
        })
        .server();

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", auth)
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "creation_disabled");
}

#[tokio::test]
async fn create_project_superusers_allows_superuser_past_gate() {
    let (db, auth) = signed_in_as(superuser());
    let db = db.append_query_results([vec![project_named("acme")]]);
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_project = CreatePermission::Superusers;
        })
        .server();

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", auth)
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::CONFLICT);
    let body: Value = res.json();
    assert_eq!(body["code"], "already_exists");
}

#[tokio::test]
async fn create_cache_superusers_rejects_regular_user() {
    let (db, auth) = signed_in();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_cache = CreatePermission::Superusers;
        })
        .server();

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", auth)
        .json(&cache_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn create_cache_none_rejects_superuser() {
    let (db, auth) = signed_in_as(superuser());
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_cache = CreatePermission::None;
        })
        .server();

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", auth)
        .json(&cache_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "creation_disabled");
}

#[tokio::test]
async fn create_cache_everyone_allows_regular_user_past_gate() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db =
        with_auth_as(mock_db(), session_id, user()).append_query_results([vec![cache_row("acme")]]);
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.permissions.create_cache = CreatePermission::Everyone;
        })
        .server();

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&cache_body())
        .await;

    res.assert_status(StatusCode::CONFLICT);
    let body: Value = res.json();
    assert_eq!(body["code"], "already_exists");
}
