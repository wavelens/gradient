/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::StatusCode;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_entity::ids::ProjectSecretId;
use gradient_entity::project_secret;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::mock::{all_statements, mock_db};
use gradient_fixtures::rows::{admin_role, project, project_member, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, make_test_server, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use uuid::Uuid;

const SECRETS_URL: &str = "/api/v1/projects/test-project/secrets";

fn secret_row(name: &str, managed: bool) -> project_secret::Model {
    project_secret::Model {
        id: ProjectSecretId::now_v7(),
        project: project_id(),
        name: name.into(),
        value: "encrypted".into(),
        managed,
        updated_by: None,
        updated_at: test_date(),
    }
}

fn as_admin() -> (MockDatabase, String) {
    let session = SessionId::now_v7();
    let db = with_auth(mock_db(), session)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]]);
    (db, bearer(session))
}

fn temp_crypt_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn a_listed_secret_shows_no_value() {
    let (db, auth) = as_admin();
    let db = db.append_query_results([vec![secret_row("wandb-token", false)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(SECRETS_URL)
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let item = &body["message"][0];
    assert_eq!(item["name"], "wandb-token");
    assert!(item.get("updated_at").is_some(), "{item}");
    assert!(item.get("value").is_none(), "{item}");
}

#[tokio::test]
async fn a_put_stores_the_value_encrypted() {
    let crypt_file = temp_crypt_file();
    let (db, auth) = as_admin();
    let db = db
        .append_query_results([Vec::<project_secret::Model>::new()])
        .append_query_results([vec![secret_row("wandb-token", false)]])
        .into_connection();
    let file = crypt_file.clone();
    let server = TestState::new()
        .web_db(db.clone())
        .cli(move |cli| cli.secrets.crypt_file = file)
        .server();

    let res = server
        .put(&format!("{SECRETS_URL}/wandb-token"))
        .add_header("authorization", auth)
        .json(&json!({ "value": "s3cr3t-value" }))
        .await;

    res.assert_status_ok();
    let insert = all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.starts_with(r#"INSERT INTO "project_secret""#))
        .unwrap();
    let stored: Vec<String> = insert
        .values
        .iter()
        .flat_map(|values| values.0.iter())
        .filter_map(|value| match value {
            sea_orm::Value::String(Some(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(!stored.iter().any(|s| s.contains("s3cr3t-value")));
    assert!(stored.iter().any(|s| {
        decrypt_secret_with_file(&crypt_file, s)
            .map(|plain| plain.expose() == "s3cr3t-value")
            .unwrap_or(false)
    }));
}

#[tokio::test]
async fn a_member_without_settings_permission_gets_403() {
    let session = SessionId::now_v7();
    let db = with_auth(mock_db(), session)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![view_role()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(SECRETS_URL)
        .add_header("authorization", bearer(session))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_managed_secret_cannot_be_replaced_or_deleted() {
    let (db, auth) = as_admin();
    let db = db.append_query_results([vec![secret_row("wandb-token", true)]]);
    let server = make_test_server(db.into_connection());
    let res = server
        .put(&format!("{SECRETS_URL}/wandb-token"))
        .add_header("authorization", auth)
        .json(&json!({ "value": "s3cr3t-value" }))
        .await;
    res.assert_status(StatusCode::CONFLICT);

    let (db, auth) = as_admin();
    let db = db.append_query_results([vec![secret_row("wandb-token", true)]]);
    let server = make_test_server(db.into_connection());
    let res = server
        .delete(&format!("{SECRETS_URL}/wandb-token"))
        .add_header("authorization", auth)
        .await;
    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_invalid_name_is_400() {
    let (db, auth) = as_admin();
    let server = make_test_server(db.into_connection());

    let res = server
        .put(&format!("{SECRETS_URL}/Wandb_Token"))
        .add_header("authorization", auth)
        .json(&json!({ "value": "s3cr3t-value" }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_sops_recipient_of_an_ed25519_project_is_an_age_key() {
    let public_key = include_str!("../../gradient-sops/tests/fixtures/ed25519.pub")
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let session = SessionId::now_v7();
    let db = with_auth(mock_db(), session)
        .append_query_results([vec![gradient_entity::project::Model {
            public_key: public_key.clone(),
            ..project()
        }]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/projects/test-project/sops-recipient")
        .add_header("authorization", bearer(session))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert!(
        body["message"]["age"].as_str().unwrap().starts_with("age1"),
        "{body}"
    );
    assert_eq!(body["message"]["ssh"], public_key);
}
