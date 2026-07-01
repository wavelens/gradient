/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the `create_org` / `create_cache` permission gate on
//! `PUT /api/v1/orgs` and `PUT /api/v1/caches` (issue #470).
//!
//! Each gate short-circuits before any DB write, so the rejection paths need
//! only the auth query chain. The allow path is proven by reaching the
//! name-taken pre-check (409) with the gate satisfied.

use axum::http::StatusCode;
use gradient_entity::{cache, organization};
use gradient_entity::ids::*;
use gradient_types::{CreatePermission, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};
use gradient_test_support::fixtures::{superuser_user, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server_configured, make_token};

fn with_auth(db: MockDatabase, session_id: SessionId, actor: gradient_entity::user::Model) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![actor]])
}

fn org_row(name: &str) -> organization::Model {
    organization::Model {
        id: OrganizationId::now_v7(),
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

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn org_body() -> Value {
    json!({ "name": "acme", "display_name": "Acme", "description": "", "public": false })
}

fn cache_body() -> Value {
    json!({
        "name": "acme", "display_name": "Acme", "description": "",
        "priority": 40, "local_priority": 40, "public": false,
    })
}

#[test]
fn create_org_superusers_rejects_regular_user() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id, user());
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_org = CreatePermission::Superusers;
        });

        let res = server
            .put("/api/v1/orgs")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&org_body())
            .await;

        res.assert_status(StatusCode::FORBIDDEN);
        let body: Value = res.json();
        assert_eq!(body["code"], "superuser_required");
    });
}

#[test]
fn create_org_none_rejects_superuser() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser_user(),
        );
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_org = CreatePermission::None;
        });

        let res = server
            .put("/api/v1/orgs")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&org_body())
            .await;

        res.assert_status(StatusCode::FORBIDDEN);
        let body: Value = res.json();
        assert_eq!(body["code"], "creation_disabled");
    });
}

#[test]
fn create_org_superusers_allows_superuser_past_gate() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser_user(),
        )
        .append_query_results([vec![org_row("acme")]]);
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_org = CreatePermission::Superusers;
        });

        let res = server
            .put("/api/v1/orgs")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&org_body())
            .await;

        res.assert_status(StatusCode::CONFLICT);
        let body: Value = res.json();
        assert_eq!(body["code"], "already_exists");
    });
}

#[test]
fn create_cache_superusers_rejects_regular_user() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id, user());
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_cache = CreatePermission::Superusers;
        });

        let res = server
            .put("/api/v1/caches")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&cache_body())
            .await;

        res.assert_status(StatusCode::FORBIDDEN);
        let body: Value = res.json();
        assert_eq!(body["code"], "superuser_required");
    });
}

#[test]
fn create_cache_none_rejects_superuser() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser_user(),
        );
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_cache = CreatePermission::None;
        });

        let res = server
            .put("/api/v1/caches")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&cache_body())
            .await;

        res.assert_status(StatusCode::FORBIDDEN);
        let body: Value = res.json();
        assert_eq!(body["code"], "creation_disabled");
    });
}

#[test]
fn create_cache_everyone_allows_regular_user_past_gate() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id, user())
            .append_query_results([vec![cache_row("acme")]]);
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_cache = CreatePermission::Everyone;
        });

        let res = server
            .put("/api/v1/caches")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&cache_body())
            .await;

        res.assert_status(StatusCode::CONFLICT);
        let body: Value = res.json();
        assert_eq!(body["code"], "already_exists");
    });
}
