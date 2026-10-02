/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::{cache, project};
use gradient_test_support::fixtures::{superuser_user, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server_configured, make_token};
use gradient_types::{CreatePermission, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};

fn with_auth(
    db: MockDatabase,
    session_id: SessionId,
    actor: gradient_entity::user::Model,
) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![actor]])
}

fn project_row(name: &str) -> project::Model {
    project::Model {
        id: ProjectId::now_v7(),
        name: name.to_string(),
        display_name: format!("{} display", name),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

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
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        user(),
    );
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_project = CreatePermission::Superusers;
    });

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn create_project_none_rejects_superuser() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        superuser_user(),
    );
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_project = CreatePermission::None;
    });

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "creation_disabled");
}

#[tokio::test]
async fn create_project_superusers_allows_superuser_past_gate() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        superuser_user(),
    )
    .append_query_results([vec![project_row("acme")]]);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_project = CreatePermission::Superusers;
    });

    let res = server
        .put("/api/v1/projects")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&project_body())
        .await;

    res.assert_status(StatusCode::CONFLICT);
    let body: Value = res.json();
    assert_eq!(body["code"], "already_exists");
}

#[tokio::test]
async fn create_cache_superusers_rejects_regular_user() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        user(),
    );
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_cache = CreatePermission::Superusers;
    });

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&cache_body())
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "superuser_required");
}

#[tokio::test]
async fn create_cache_none_rejects_superuser() {
    let session_id = SessionId::now_v7();
    let token = make_token(session_id);
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        superuser_user(),
    );
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_cache = CreatePermission::None;
    });

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", format!("Bearer {}", token))
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
    let db = with_auth(
        MockDatabase::new(DatabaseBackend::Postgres),
        session_id,
        user(),
    )
    .append_query_results([vec![cache_row("acme")]]);
    let server = make_test_server_configured(db.into_connection(), |cli| {
        cli.permissions.create_cache = CreatePermission::Everyone;
    });

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&cache_body())
        .await;

    res.assert_status(StatusCode::CONFLICT);
    let body: Value = res.json();
    assert_eq!(body["code"], "already_exists");
}
