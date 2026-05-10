/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for project-level outbound integration linking
//! (`PUT /projects/{org}/{project}/integration`) and the protections that keep
//! the auto-managed `forge_type=github` rows safe from edits or deletes.

use entity::{ids::*, integration, organization_user, project, project_integration};
use gradient_core::types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use test_support::fixtures::{org, org_id, project_id, test_date, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixture helpers ────────────────────────────────────────────────────────────

fn outbound_integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000077").unwrap())
}

fn github_outbound_row() -> integration::Model {
    integration::Model {
        id: outbound_integration_id(),
        organization: org_id(),
        name: "github".into(),
        display_name: "GitHub".into(),
        kind: 1,       // Outbound
        forge_type: 3, // GitHub
        secret: None,
        endpoint_url: None,
        access_token: None,
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn gitea_outbound_row() -> integration::Model {
    integration::Model {
        id: outbound_integration_id(),
        organization: org_id(),
        name: "my-gitea".into(),
        display_name: "My Gitea".into(),
        kind: 1,       // Outbound
        forge_type: 0, // Gitea
        secret: None,
        endpoint_url: Some("https://gitea.example.com".into()),
        access_token: Some("encrypted-token".into()),
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn project_row() -> project::Model {
    project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
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
        organization: None,
        permission: gradient_core::permissions::admin_mask(),
        managed: false,
    }
}

fn link_pointing_at(integration_id: IntegrationId) -> project_integration::Model {
    project_integration::Model {
        project: project_id(),
        outbound_integration: Some(integration_id),
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

/// Append a `load_project` Require(EditProject) sequence: org+project, org_user, role.
fn with_project_edit(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

/// Append a `load_org` Require(ManageIntegrations) sequence: org, org_user, role.
fn with_org_admin(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role_row()]])
}

const PROJECT_INTEGRATION_URL: &str = "/api/v1/projects/test-org/test-project/integration";

// ── Tests: linking the github outbound row ─────────────────────────────────────

#[test]
fn put_project_integration_accepts_github_outbound_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let int_id = outbound_integration_id();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // validate_integration: SELECT integration row
        .append_query_results([vec![github_outbound_row()]])
        // existing project_integration row: none
        .append_query_results([Vec::<project_integration::Model>::new()])
        // insert returning
        .append_query_results([vec![link_pointing_at(int_id)]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put(PROJECT_INTEGRATION_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "outbound_integration": int_id.to_string(),
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"]["outbound_integration"], int_id.to_string());
    });
}

#[test]
fn put_project_integration_accepts_non_github_outbound_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let int_id = outbound_integration_id();

        let db = with_project_edit(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![gitea_outbound_row()]])
        .append_query_results([Vec::<project_integration::Model>::new()])
        .append_query_results([vec![link_pointing_at(int_id)]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .put(PROJECT_INTEGRATION_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "outbound_integration": int_id.to_string(),
            }))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}

// ── Tests: github rows are server-managed ─────────────────────────────────────

#[test]
fn patch_integration_rejects_github_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let int_id = outbound_integration_id();

        let db = with_org_admin(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        // load_integration_in_org
        .append_query_results([vec![github_outbound_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .patch(&format!("/api/v1/orgs/test-org/integrations/{}", int_id))
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "display_name": "Renamed" }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        let msg = body["message"].as_str().unwrap().to_lowercase();
        assert!(
            msg.contains("github") && msg.contains("manag"),
            "expected managed/github error, got: {msg}"
        );
    });
}

#[test]
fn delete_integration_rejects_github_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let int_id = outbound_integration_id();

        let db = with_org_admin(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![github_outbound_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("/api/v1/orgs/test-org/integrations/{}", int_id))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
    });
}

#[test]
fn delete_integration_accepts_non_github_row() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let int_id = outbound_integration_id();

        let db = with_org_admin(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![gitea_outbound_row()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);

        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!("/api/v1/orgs/test-org/integrations/{}", int_id))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
    });
}

#[test]
fn put_integration_rejects_reserved_github_name() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_admin(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/orgs/test-org/integrations")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "github",
                "kind": "outbound",
                "forge_type": "gitea",
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        let msg = body["message"].as_str().unwrap().to_lowercase();
        assert!(
            msg.contains("reserved"),
            "expected reserved-name error, got: {msg}"
        );
    });
}

#[test]
fn put_integration_still_rejects_github_forge_type() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_admin(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/orgs/test-org/integrations")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "manual-github",
                "kind": "outbound",
                "forge_type": "github",
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        let msg = body["message"].as_str().unwrap().to_lowercase();
        assert!(
            msg.contains("github"),
            "expected github mention, got: {msg}"
        );
    });
}
