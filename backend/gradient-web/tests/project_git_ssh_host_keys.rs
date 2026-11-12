/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::ProjectSshHostKeyId;
use gradient_entity::project_ssh_host_key;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::mock::{all_statements, mock_db};
use gradient_fixtures::rows::{admin_role, project, project_member, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::{bearer, make_test_server, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use sea_orm::MockDatabase;
use serde_json::json;

const PROJECT_GIT_SSH_HOST_KEYS_URL: &str = "/api/v1/projects/test-project/ssh/host-keys";

fn project_ssh_host_key_row_for_host_port_and_public_key(
    host: &str,
    port: i32,
    public_key: &str,
) -> project_ssh_host_key::Model {
    project_ssh_host_key::Model {
        id: ProjectSshHostKeyId::now_v7(),
        project: project_id(),
        host: host.into(),
        port,
        public_key: public_key.into(),
        learned_at: test_date(),
        changed_at: None,
    }
}

fn ed25519_public_key() -> String {
    include_str!("../../gradient-sops/tests/fixtures/ed25519.pub")
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn as_admin() -> (MockDatabase, String) {
    let session = SessionId::now_v7();
    let db = with_auth(mock_db(), session)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]]);
    (db, bearer(session))
}

#[tokio::test]
async fn saving_a_host_key_with_an_rsa_key_is_rejected() {
    let (db, auth) = as_admin();
    let server = make_test_server(db.into_connection());

    let res = server
        .put(PROJECT_GIT_SSH_HOST_KEYS_URL)
        .add_header("authorization", auth)
        .json(&json!({
            "host": "git.elektrat.de",
            "port": 22,
            "public_key": include_str!("../../gradient-sops/tests/fixtures/rsa.pub").trim(),
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn saving_a_host_key_lowercases_the_host() {
    let public_key = ed25519_public_key();
    let (db, auth) = as_admin();
    let db = db
        .append_query_results([vec![project_ssh_host_key_row_for_host_port_and_public_key(
            "git.elektrat.de",
            22,
            &public_key,
        )]])
        .into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .put(PROJECT_GIT_SSH_HOST_KEYS_URL)
        .add_header("authorization", auth)
        .json(&json!({ "host": "Git.Elektrat.de", "port": 22, "public_key": public_key }))
        .await;

    res.assert_status_ok();
    let insert = all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.starts_with(r#"INSERT INTO "project_ssh_host_key""#))
        .unwrap();
    let hosts: Vec<String> = insert
        .values
        .iter()
        .flat_map(|values| values.0.iter())
        .filter_map(|value| match value {
            sea_orm::Value::String(Some(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(hosts.iter().any(|s| s == "git.elektrat.de"), "{hosts:?}");
    assert!(!hosts.iter().any(|s| s == "Git.Elektrat.de"), "{hosts:?}");
}

#[tokio::test]
async fn a_member_without_ssh_key_permission_cannot_delete_a_host_key() {
    let session = SessionId::now_v7();
    let db = with_auth(mock_db(), session)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![view_role()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .delete(&format!(
            "{PROJECT_GIT_SSH_HOST_KEYS_URL}/{}",
            ProjectSshHostKeyId::now_v7()
        ))
        .add_header("authorization", bearer(session))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}
