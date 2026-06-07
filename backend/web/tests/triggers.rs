/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for project_trigger CRUD endpoints.
//!
//! Pattern: manual Tokio runtime + `axum_test::TestServer` + `MockDatabase`.
//! Uses manual runtimes because `#[tokio::test]` expands to `::gradient_core::…`
//! which clashes with the local `core` crate name in this workspace.
//!
//! Auth sequence per request through `authorize` middleware (in order):
//!   1. SELECT session (by jti)
//!   2. UPDATE session (last_used_at)
//!   3. SELECT user (by id)
//!
//! Then per `load_project`:
//!   4. SELECT org (by name)
//!   5. SELECT project (by org + name)
//!   6. SELECT org_user membership (permission check)

use entity::{ids::*, integration, organization_user, project, project_trigger};
use gradient_core::types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixture helpers ────────────────────────────────────────────────────────────

fn trigger_id() -> ProjectTriggerId {
    ProjectTriggerId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap())
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

fn admin_membership() -> organization_user::Model {
    organization_user::Model {
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

fn polling_trigger_row() -> project_trigger::Model {
    project_trigger::Model {
        id: trigger_id(),
        project: project_id(),
        config: serde_json::json!({"interval_secs": 60}),
        active: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn github_integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("019e16b2-e958-7652-ad97-67cd7b0fea61").unwrap())
}

fn github_inbound_integration_row() -> integration::Model {
    integration::Model {
        id: github_integration_id(),
        organization: org_id(),
        name: "github".into(),
        display_name: "GitHub".into(),
        forge_type: 3, // GitHub
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn reporter_push_trigger_row() -> project_trigger::Model {
    project_trigger::Model {
        id: trigger_id(),
        project: project_id(),
        trigger_type: 1, // ReporterPush
        config: serde_json::json!({
            "integration_id": github_integration_id().to_string(),
            "branches": ["main"],
            "tags": [],
            "releases_only": false,
        }),
        active: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

/// Append the standard auth mock sequence:
/// 1. SELECT session (decode_jwt validates session)
/// 2. SELECT session (UPDATE ... RETURNING - Postgres backend uses RETURNING path)
/// 3. SELECT user
fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

/// Append a `load_project` sequence with Member access (no permission row needed):
/// 1. SELECT org
/// 2. SELECT project
/// 3. SELECT org_user (membership check)
fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
}

/// Append a `load_project` sequence with Require(EditProject) access:
/// 1. SELECT org
/// 2. SELECT project
/// 3. SELECT org_user (permission check)
/// 4. SELECT role (bitmask lookup behind `mask_grants`)
fn with_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

const BASE_URL: &str = "/api/v1/projects/test-org/test-project/triggers";

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
fn list_triggers_returns_rows() {
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
        .append_query_results([vec![polling_trigger_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        let items = body["message"].as_array().expect("message is array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "polling");
        assert_eq!(items[0]["active"], true);
    });
}

#[test]
fn get_trigger_returns_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![polling_trigger_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "polling");
        assert_eq!(body["message"]["id"], tid.to_string());
    });
}

#[test]
fn get_trigger_not_found_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = ProjectTriggerId::now_v7();

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_trigger::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn create_polling_trigger_valid() {
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
        .append_query_results([vec![polling_trigger_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "polling", "interval_secs": 60}
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "polling");
    });
}

#[test]
fn create_polling_trigger_interval_too_small_returns_400() {
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
            .json(&serde_json::json!({
                "config": {"type": "polling", "interval_secs": 5}
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        let msg = body["message"].as_str().unwrap();
        assert!(
            msg.contains("interval_secs"),
            "expected interval message, got: {msg}"
        );
    });
}

#[test]
fn create_invalid_cron_returns_400() {
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
            .json(&serde_json::json!({
                "config": {"type": "time", "cron": "not a cron"}
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn patch_trigger_updates_fields() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let updated = polling_trigger_row();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![polling_trigger_row()]])
        .append_query_results([vec![updated]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"active": false}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "polling");
    });
}

#[test]
fn patch_trigger_config_type_change() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let updated = project_trigger::Model {
            trigger_type: 3, // Time
            config: serde_json::json!({"cron": "0 0 2 * * *"}),
            ..polling_trigger_row()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![polling_trigger_row()]])
        .append_query_results([vec![updated]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "config": {"type": "time", "cron": "0 0 2 * * *"}
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["type"], "time");
    });
}

#[test]
fn delete_trigger_removes_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![polling_trigger_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["deleted"], true);
    });
}

#[test]
fn delete_trigger_not_found_returns_404() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = ProjectTriggerId::now_v7();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([Vec::<project_trigger::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn fire_now_on_inactive_trigger_returns_400() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let inactive_trigger = project_trigger::Model {
            active: false,
            ..polling_trigger_row()
        };

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![inactive_trigger]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .post(&format!("{}/{}/test", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(
            body["message"].as_str().unwrap().contains("inactive"),
            "expected inactive mention, got: {}",
            body["message"]
        );
    });
}

// fire_now is not integration-tested further here because it calls resolve_head
// which makes actual git network requests - it will be exercised by E2E smoke tests.

#[test]
fn create_project_seeds_default_polling_trigger() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let created_project = project::Model {
            id: project_id(),
            organization: org_id(),
            name: "new-project".into(),
            active: true,
            display_name: "New Project".into(),
            repository: "https://github.com/test/repo".into(),
            wildcard: "*".into(),
            last_check_at: test_date(),
            created_by: user_id(),
            created_at: test_date(),
            keep_evaluations: 30,
            concurrency: 3,
            sign_cache: true,
            ..Default::default()
        };

        let seeded_trigger = project_trigger::Model {
            id: trigger_id(),
            project: project_id(),
            config: serde_json::json!({"interval_secs": 300}),
            active: true,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            // load_org: SELECT org
            .append_query_results([vec![org()]])
            // load_org: SELECT org_user (require CreateProject permission)
            .append_query_results([vec![admin_membership()]])
            // load_org: SELECT role (bitmask lookup)
            .append_query_results([vec![admin_role_row()]])
            // check existing project: returns empty
            .append_query_results([Vec::<project::Model>::new()])
            // INSERT project RETURNING
            .append_query_results([vec![created_project]])
            // INSERT trigger RETURNING
            .append_query_results([vec![seeded_trigger]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-org")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "new-project",
                "display_name": "New Project",
                "description": "",
                "repository": "https://github.com/test/repo",
                "wildcard": "*"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], project_id().to_string());
    });
}

#[test]
fn create_project_with_all_concurrency_returns_id() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let created_project = project::Model {
            id: project_id(),
            organization: org_id(),
            name: "new-project".into(),
            active: true,
            display_name: "New Project".into(),
            repository: "https://github.com/test/repo".into(),
            wildcard: "*".into(),
            last_check_at: test_date(),
            created_by: user_id(),
            created_at: test_date(),
            keep_evaluations: 30,
            concurrency: 2, // All
            sign_cache: true,
            ..Default::default()
        };

        let seeded_trigger = entity::project_trigger::Model {
            id: trigger_id(),
            project: project_id(),
            config: serde_json::json!({"interval_secs": 300}),
            active: true,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([Vec::<project::Model>::new()])
            .append_query_results([vec![created_project]])
            .append_query_results([vec![seeded_trigger]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-org")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "new-project",
                "display_name": "New Project",
                "description": "",
                "repository": "https://github.com/test/repo",
                "wildcard": "*",
                "concurrency": "all"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], project_id().to_string());
    });
}

#[test]
fn create_project_with_hard_abort_concurrency_returns_id() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let created_project = project::Model {
            id: project_id(),
            organization: org_id(),
            name: "new-project".into(),
            active: true,
            display_name: "New Project".into(),
            repository: "https://github.com/test/repo".into(),
            wildcard: "*".into(),
            last_check_at: test_date(),
            created_by: user_id(),
            created_at: test_date(),
            keep_evaluations: 30,
            sign_cache: true,
            ..Default::default()
        };

        let seeded_trigger = entity::project_trigger::Model {
            id: trigger_id(),
            project: project_id(),
            config: serde_json::json!({"interval_secs": 300}),
            active: true,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([vec![admin_membership()]])
            .append_query_results([vec![admin_role_row()]])
            .append_query_results([Vec::<project::Model>::new()])
            .append_query_results([vec![created_project]])
            .append_query_results([vec![seeded_trigger]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-org")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "new-project",
                "display_name": "New Project",
                "description": "",
                "repository": "https://github.com/test/repo",
                "wildcard": "*",
                "concurrency": "hard_abort"
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], project_id().to_string());
    });
}

#[test]
fn patch_project_concurrency_to_skip() {
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
        // aproject.update() - read-back then exec
        .append_query_results([vec![project_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch("/api/v1/projects/test-org/test-project")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"concurrency": "skip"}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}

#[test]
fn patch_project_all_concurrency_returns_ok() {
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
        .append_query_results([vec![project_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch("/api/v1/projects/test-org/test-project")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"concurrency": "all"}))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}

// ── Integration enrichment tests ──────────────────────────────────────────────
//
// Regression coverage: reporter triggers must surface the referenced
// integration's name/display_name/forge_type alongside the raw `integration_id`,
// so the trigger UI can render "from GitHub" instead of falling back to a UUID.
// Polling triggers must keep `integration: null` (no extra DB round-trip).

#[test]
fn list_reporter_trigger_includes_integration_metadata() {
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
        .append_query_results([vec![reporter_push_trigger_row()]])
        .append_query_results([vec![github_inbound_integration_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let item = &body["message"][0];
        assert_eq!(item["type"], "reporter_push");
        assert_eq!(
            item["integration"]["id"],
            github_integration_id().to_string()
        );
        assert_eq!(item["integration"]["name"], "github");
        assert_eq!(item["integration"]["display_name"], "GitHub");
        assert_eq!(item["integration"]["forge_type"], "github");
    });
}

#[test]
fn list_reporter_trigger_with_missing_integration_returns_null() {
    // Trigger row references an integration ID that no longer exists in the
    // org (row was deleted). Response keeps the trigger but sets
    // `integration: null` so the UI can degrade gracefully.
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
        .append_query_results([vec![reporter_push_trigger_row()]])
        .append_query_results([Vec::<integration::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"][0]["type"], "reporter_push");
        assert!(body["message"][0]["integration"].is_null());
    });
}

#[test]
fn list_polling_trigger_has_null_integration_and_skips_lookup() {
    // No reporter triggers in the list - handler must NOT issue an integration
    // SELECT. MockDatabase panics on unexpected queries, which is the assertion.
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
        .append_query_results([vec![polling_trigger_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(BASE_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"][0]["type"], "polling");
        assert!(body["message"][0]["integration"].is_null());
    });
}

#[test]
fn get_reporter_trigger_includes_integration_metadata() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let tid = trigger_id();

        let db = with_project_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![reporter_push_trigger_row()]])
        .append_query_results([vec![github_inbound_integration_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(&format!("{}/{}", BASE_URL, tid))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"]["type"], "reporter_push");
        assert_eq!(body["message"]["integration"]["display_name"], "GitHub");
        assert_eq!(body["message"]["integration"]["forge_type"], "github");
    });
}
