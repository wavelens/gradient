/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /api/v1/admin/state`.
//!
//! The export handler is superuser-gated and supports two output formats. These
//! tests cover the auth gate, format validation, and the empty-database render
//! for both formats. A full round-trip against real data lives in the nix api
//! integration test (`nix/tests/gradient/api`).

use gradient_core::types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use gradient_test_support::fixtures::user;
use gradient_test_support::web::{live_session, make_test_server, make_token};

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn superuser() -> gradient_entity::user::Model {
    gradient_entity::user::Model {
        superuser: true,
        ..user()
    }
}

/// Append the two session lookups plus the caller's user row consumed by the
/// auth middleware.
fn with_user(db: MockDatabase, session_id: SessionId, caller: gradient_entity::user::Model) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![caller]])
}

/// Append the sixteen (all-empty) table reads `export_state` issues, in order.
fn with_empty_export(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::user::Model>::new()])
        .append_query_results([Vec::<gradient_entity::organization::Model>::new()])
        .append_query_results([Vec::<gradient_entity::project::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache::Model>::new()])
        .append_query_results([Vec::<gradient_entity::role::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_role::Model>::new()])
        .append_query_results([Vec::<gradient_entity::api::Model>::new()])
        .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
        .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
        .append_query_results([Vec::<gradient_entity::organization_user::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
        .append_query_results([Vec::<gradient_entity::organization_cache::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache_upstream::Model>::new()])
        .append_query_results([Vec::<gradient_entity::project_trigger::Model>::new()])
        .append_query_results([Vec::<gradient_entity::project_action::Model>::new()])
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
}

#[test]
fn export_state_rejects_non_superuser() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_user(MockDatabase::new(DatabaseBackend::Postgres), session_id, user());

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/admin/state")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_forbidden();
    });
}

#[test]
fn export_state_rejects_unknown_format() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_user(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser(),
        );

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/admin/state?format=yaml")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_bad_request();
    });
}

#[test]
fn export_state_json_returns_empty_shape() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_empty_export(with_user(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser(),
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/admin/state?format=json")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        for key in [
            "users",
            "organizations",
            "projects",
            "caches",
            "roles",
            "api_keys",
            "workers",
            "integrations",
        ] {
            assert!(
                body["message"][key].is_object(),
                "missing key '{key}' in export"
            );
        }
    });
}

#[test]
fn export_state_defaults_to_nix() {
    run(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let db = with_empty_export(with_user(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
            superuser(),
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .get("/api/v1/admin/state")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        assert!(
            res.header("content-type")
                .to_str()
                .unwrap()
                .starts_with("text/plain"),
        );
        let body = res.text();
        assert!(body.starts_with("# Generated by"));
        assert!(body.contains("users = { };"));
    });
}
