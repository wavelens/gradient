/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::integration::IntegrationKind;
use gradient_entity::{github_installation, ids::*, integration, project_user, role};
use gradient_test_support::fixtures::{project, project_id, test_date, user, user_id};
use gradient_test_support::web::{live_session, make_test_server, make_token};
use gradient_types::{GitHostType, SessionId};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde_json::Value;
use uuid::Uuid;

fn member_only_membership() -> project_user::Model {
    // `ProjectAccess::Member` is not dereferencing the role. A synthetic role id that is never
    // loaded is proving the summary endpoint is working without `ManageIntegrations`.
    project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap()),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_VIEW_ID,
    }
}

fn gitea_inbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000033").unwrap()),
        project: project_id(),
        name: "my-gitea-hook".into(),
        display_name: "My Gitea".into(),
        secret: Some("encrypted-blob".into()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn github_installation_id() -> GithubInstallationId {
    GithubInstallationId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000cc").unwrap())
}

fn github_integration_id() -> IntegrationId {
    IntegrationId::new(Uuid::parse_str("019e16b2-e958-7652-ad97-67cd7b0fea61").unwrap())
}

fn github_inbound_row() -> integration::Model {
    integration::Model {
        id: github_integration_id(),
        project: project_id(),
        name: "github".into(),
        display_name: "GitHub".into(),
        git_host_type: GitHostType::GitHub,
        github_installation: Some(github_installation_id()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn github_installation_row() -> github_installation::Model {
    github_installation::Model {
        id: github_installation_id(),
        project: project_id(),
        installation_id: 999,
        account_login: Some("acme-corp".into()),
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn gitea_outbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000044").unwrap()),
        project: project_id(),
        name: "my-gitea-reporter".into(),
        display_name: "My Gitea CI".into(),
        kind: IntegrationKind::Outbound,
        endpoint_url: Some("https://gitea.example.com".into()),
        access_token: Some("encrypted-token".into()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn with_project_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![member_only_membership()]])
}

fn admin_membership() -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
    }
}

fn admin_role() -> role::Model {
    role::Model {
        id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_db::permissions::admin_mask(),
        ..Default::default()
    }
}

fn with_project_manage(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![admin_membership()]])
        .append_query_results([vec![admin_role()]])
}

const SUMMARY_URL: &str = "/api/v1/projects/test-project/integrations/summary";

#[test]
fn summary_endpoint_returns_all_kinds() {
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
        .append_query_results([vec![
            gitea_inbound_row(),
            github_inbound_row(),
            gitea_outbound_row(),
        ]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let items = body["message"].as_array().expect("array");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["kind"], "inbound");
        assert_eq!(items[0]["git_host_type"], "gitea");
        assert_eq!(items[0]["name"], "my-gitea-hook");
        assert_eq!(items[1]["git_host_type"], "github");
        assert_eq!(items[1]["display_name"], "GitHub");
        assert_eq!(items[2]["kind"], "outbound");
    });
}

#[test]
fn summary_endpoint_excludes_credential_state() {
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
        .append_query_results([vec![gitea_inbound_row(), gitea_outbound_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        for item in body["message"].as_array().unwrap() {
            let obj = item.as_object().unwrap();
            for forbidden in [
                "secret",
                "endpoint_url",
                "access_token",
                "has_secret",
                "has_access_token",
            ] {
                assert!(
                    !obj.contains_key(forbidden),
                    "summary leaked `{forbidden}`: {obj:?}"
                );
            }
        }
    });
}

#[test]
fn summary_endpoint_rejects_non_member() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![project()]])
            .append_query_results([Vec::<project_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}

#[test]
fn delete_github_integration_removes_pair_and_installation() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);
        let integration_id = github_integration_id();

        let db = with_project_manage(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![github_inbound_row()]])
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
        ]);

        let _ = github_installation_row();
        let server = make_test_server(db.into_connection());
        let res = server
            .delete(&format!(
                "/api/v1/projects/test-project/integrations/{}",
                integration_id
            ))
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["error"], false);
        assert_eq!(body["message"], true);
    });
}

#[test]
fn github_create_without_app_config_is_rejected() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_project_manage(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ));

        let server = make_test_server(db.into_connection());
        let res = server
            .put("/api/v1/projects/test-project/integrations")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "my-gh",
                "kind": "outbound",
                "git_host_type": "github",
                "installation_id": 42,
            }))
            .await;

        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], true);
        assert!(
            body["message"].as_str().unwrap().contains("not configured"),
            "expected 'not configured' in error: {}",
            body["message"]
        );
    });
}
