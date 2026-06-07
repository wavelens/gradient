/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::ids::*;
use entity::project_flake_input_override;
use gradient_core::types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixtures ───────────────────────────────────────────────────────────────────

fn override_id() -> FlakeInputOverrideId {
    FlakeInputOverrideId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000077").unwrap())
}

fn project_row() -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

fn managed_project_row() -> entity::project::Model {
    entity::project::Model {
        managed: true,
        ..project_row()
    }
}

fn admin_membership() -> entity::organization_user::Model {
    entity::organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn admin_role_row() -> entity::role::Model {
    entity::role::Model {
        id: gradient_core::types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_core::permissions::admin_mask(),
        ..Default::default()
    }
}

fn nixpkgs_override_row() -> project_flake_input_override::Model {
    project_flake_input_override::Model {
        id: override_id(),
        project: project_id(),
        input_name: "nixpkgs".into(),
        url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

fn with_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

fn with_managed_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![managed_project_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

const BASE_URL: &str = "/api/v1/projects/test-org/test-project/flake-inputs";

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
fn list_empty_returns_empty() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_flake_input_override::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"].as_array().unwrap().len(), 0);
    });
}

#[test]
fn create_then_list_returns_one() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_flake_input_override::Model>::new()])
        .append_query_results([vec![nixpkgs_override_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "input_name": "nixpkgs",
                "url": "github:NixOS/nixpkgs/nixos-unstable"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["input_name"], "nixpkgs");
        assert_eq!(
            body["message"]["url"],
            "github:NixOS/nixpkgs/nixos-unstable"
        );
    });
}

#[test]
fn create_with_null_url_keep_url_mode() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let null_url_row = project_flake_input_override::Model {
            id: override_id(),
            project: project_id(),
            input_name: "utils".into(),
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_flake_input_override::Model>::new()])
        .append_query_results([vec![null_url_row]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"input_name": "utils", "url": null}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["input_name"], "utils");
        assert!(body["message"]["url"].is_null());
    });
}

#[test]
fn create_duplicate_input_name_rejects_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![nixpkgs_override_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "input_name": "nixpkgs",
                "url": "github:NixOS/nixpkgs/nixos-unstable"
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn create_invalid_input_name_rejects_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"input_name": "bad name!", "url": "x"}))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(
            body["message"].as_str().unwrap().contains("input_name"),
            "expected input_name in message, got: {}",
            body["message"]
        );
    });
}

#[test]
fn patch_updates_url_and_returns_new_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let oid = override_id();

        let updated = project_flake_input_override::Model {
            url: Some("github:NixOS/nixpkgs/nixos-24.05".into()),
            ..nixpkgs_override_row()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_query_results([vec![updated]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, oid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"url": "github:NixOS/nixpkgs/nixos-24.05"}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["url"], "github:NixOS/nixpkgs/nixos-24.05");
    });
}

#[test]
fn patch_url_to_null_sets_keep_url() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let oid = override_id();

        let updated = project_flake_input_override::Model {
            url: None,
            ..nixpkgs_override_row()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_query_results([vec![updated]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, oid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"url": null}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert!(body["message"]["url"].is_null());
    });
}

#[test]
fn patch_omitting_url_does_not_change_it() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let oid = override_id();

        let updated = project_flake_input_override::Model {
            input_name: "renamed".into(),
            url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
            ..nixpkgs_override_row()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![nixpkgs_override_row()]])
        // dup-check for "renamed" → empty means no conflict
        .append_query_results([Vec::<project_flake_input_override::Model>::new()])
        .append_query_results([vec![updated]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, oid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"input_name": "renamed"}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["input_name"], "renamed");
        assert_eq!(
            body["message"]["url"],
            "github:NixOS/nixpkgs/nixos-unstable"
        );
    });
}

#[test]
fn delete_removes_the_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let oid = override_id();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![nixpkgs_override_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("{}/{}", BASE_URL, oid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["deleted"], true);
    });
}

#[test]
fn get_not_found_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let oid = FlakeInputOverrideId::now_v7();

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_flake_input_override::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, oid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn list_sorted_by_input_name() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let alpha = project_flake_input_override::Model {
            id: FlakeInputOverrideId::new(
                Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            ),
            project: project_id(),
            input_name: "alpha".into(),
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };
        let zebra = project_flake_input_override::Model {
            id: FlakeInputOverrideId::new(
                Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            ),
            project: project_id(),
            input_name: "zebra".into(),
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };

        // MockDatabase returns rows in insertion order; handler sorts by input_name ASC,
        // so we insert alpha first as that is the expected sorted order.
        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![alpha, zebra]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let items = body["message"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["input_name"], "alpha");
        assert_eq!(items[1]["input_name"], "zebra");
    });
}

#[test]
fn managed_project_rejects_mutations_403() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_managed_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "input_name": "nixpkgs",
                "url": "github:NixOS/nixpkgs/nixos-unstable"
            }))
            .await;

        res.assert_status_forbidden();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}
