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
use axum_test::TestServer;
use gradient_entity::{project, role};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::SessionId;
use gradient_types::permissions::is_builtin_role;
use serde_json::{Value, json};
use uuid::Uuid;

const URL: &str = "/api/v1/projects/test-project/git/https-credentials";

struct TestSetup {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    crypt_file: String,
}

async fn create_test_database_with_project_membership_for_role_and_start_web_test_server(
    member_role: role::Model,
    managed: bool,
) -> TestSetup {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    let session = SessionId::now_v7();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&conn, rows::user())
        .await
        .unwrap();
    gradient_db::rows::insert::<gradient_entity::session::Entity, _>(
        &conn,
        rows::live_session(session),
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<project::Entity, _>(
        &conn,
        project::Model {
            managed,
            ..rows::project()
        },
    )
    .await
    .unwrap();
    let mut roles = vec![rows::admin_role(), rows::write_role(), rows::view_role()];
    if !is_builtin_role(member_role.id) {
        roles.push(member_role.clone());
    }
    for role in roles {
        gradient_db::rows::insert::<role::Entity, _>(&conn, role)
            .await
            .unwrap();
    }
    gradient_db::rows::insert::<gradient_entity::project_user::Entity, _>(
        &conn,
        rows::project_member(member_role.id),
    )
    .await
    .unwrap();

    let crypt_file = std::env::temp_dir()
        .join(format!("gradient-test-crypt-{}", Uuid::now_v7()))
        .to_string_lossy()
        .into_owned();
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let file = crypt_file.clone();
    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn)
        .cli(move |cli| cli.secrets.crypt_file = file)
        .server();
    TestSetup {
        db,
        server,
        auth: bearer(session),
        crypt_file,
    }
}

async fn read_all_https_git_credential_rows_for_test_project(
    s: &TestSetup,
) -> Vec<gradient_types::MProjectGitHttpsCredential> {
    gradient_db::project_git_https_credentials::list(&s.db.connection(), project_id())
        .await
        .unwrap()
}

#[tokio::test]
async fn list_https_git_credentials_returns_host_username_updated_at_without_password() {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        false,
    )
    .await;
    s.server
        .put(URL)
        .add_header("authorization", s.auth.clone())
        .json(&json!({ "host": "github.com", "username": "x-access-token", "password": "ghp_s3cr3t" }))
        .await
        .assert_status_ok();

    let res = s
        .server
        .get(URL)
        .add_header("authorization", s.auth.clone())
        .await;

    res.assert_status_ok();
    assert!(!res.text().contains("ghp_s3cr3t"));
    let body: Value = res.json();
    let item = &body["message"][0];
    assert_eq!(item["host"], "github.com");
    assert_eq!(item["username"], "x-access-token");
    assert!(item.get("updated_at").is_some(), "{item}");
    assert!(item.get("password").is_none(), "{item}");
    s.db.finish().await;
}

#[tokio::test]
async fn https_git_credential_stores_encrypted_password_and_lowercase_host() {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::write_role(),
        false,
    )
    .await;

    s.server
        .put(URL)
        .add_header("authorization", s.auth.clone())
        .json(&json!({ "host": "GitHub.com:443", "username": "ci", "password": "ghp_s3cr3t" }))
        .await
        .assert_status_ok();

    let rows = read_all_https_git_credential_rows_for_test_project(&s).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].host, "github.com");
    assert_ne!(rows[0].password, "ghp_s3cr3t");
    let decrypted = gradient_sources::decrypt_secret(&s.crypt_file, &rows[0].password).unwrap();
    assert_eq!(decrypted.expose(), "ghp_s3cr3t");
    s.db.finish().await;
}

#[tokio::test]
async fn saving_https_git_credential_with_host_containing_path_scheme_user_or_ipv6_returns_400_and_stores_nothing()
 {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        false,
    )
    .await;

    for host in [
        "https://github.com",
        "github.com/org",
        "user@github.com",
        "[::1]:8443",
    ] {
        s.server
            .put(URL)
            .add_header("authorization", s.auth.clone())
            .json(&json!({ "host": host, "username": "ci", "password": "ghp_s3cr3t" }))
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    assert!(
        read_all_https_git_credential_rows_for_test_project(&s)
            .await
            .is_empty()
    );
    s.db.finish().await;
}

#[tokio::test]
async fn project_member_with_view_role_without_manage_ssh_key_permission_gets_403_saving_https_git_credential()
 {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::view_role(),
        false,
    )
    .await;

    s.server
        .put(URL)
        .add_header("authorization", s.auth.clone())
        .json(&json!({ "host": "github.com", "username": "ci", "password": "ghp_s3cr3t" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    assert!(
        read_all_https_git_credential_rows_for_test_project(&s)
            .await
            .is_empty()
    );
    s.db.finish().await;
}

#[tokio::test]
async fn declarative_state_managed_project_refuses_saving_and_deleting_https_git_credentials_with_403()
 {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        true,
    )
    .await;
    let existing = gradient_db::project_git_https_credentials::upsert(
        &s.db.connection(),
        project_id(),
        "github.com",
        "ci",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();

    s.server
        .put(URL)
        .add_header("authorization", s.auth.clone())
        .json(&json!({ "host": "github.com", "username": "other", "password": "ghp_s3cr3t" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    s.server
        .delete(&format!("{URL}/{}", existing.id))
        .add_header("authorization", s.auth.clone())
        .await
        .assert_status(StatusCode::FORBIDDEN);

    let rows = read_all_https_git_credential_rows_for_test_project(&s).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].username, "ci");
    s.db.finish().await;
}

#[tokio::test]
async fn saving_https_git_credential_with_empty_or_whitespace_username_or_password_returns_400_and_stores_nothing()
 {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        false,
    )
    .await;

    for (username, password) in [
        ("", "ghp_s3cr3t"),
        ("  ", "ghp_s3cr3t"),
        ("ci", ""),
        ("ci", " \t\n"),
    ] {
        s.server
            .put(URL)
            .add_header("authorization", s.auth.clone())
            .json(&json!({ "host": "github.com", "username": username, "password": password }))
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    assert!(
        read_all_https_git_credential_rows_for_test_project(&s)
            .await
            .is_empty()
    );
    s.db.finish().await;
}

#[tokio::test]
async fn delete_https_git_credential_of_other_project_returns_404_and_keeps_credential() {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        false,
    )
    .await;
    let other = gradient_db::rows::insert::<project::Entity, _>(
        &s.db.connection(),
        rows::project_named("other-project"),
    )
    .await
    .unwrap();
    let foreign = gradient_db::project_git_https_credentials::upsert(
        &s.db.connection(),
        other.id,
        "github.com",
        "ci",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();

    s.server
        .delete(&format!("{URL}/{}", foreign.id))
        .add_header("authorization", s.auth.clone())
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let rows = gradient_db::project_git_https_credentials::list(&s.db.connection(), other.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, foreign.id);
    s.db.finish().await;
}

#[tokio::test]
async fn delete_https_git_credential_by_id_removes_it_from_list() {
    let s = create_test_database_with_project_membership_for_role_and_start_web_test_server(
        rows::admin_role(),
        false,
    )
    .await;
    let existing = gradient_db::project_git_https_credentials::upsert(
        &s.db.connection(),
        project_id(),
        "github.com",
        "ci",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();

    s.server
        .delete(&format!("{URL}/{}", existing.id))
        .add_header("authorization", s.auth.clone())
        .await
        .assert_status_ok();

    assert!(
        read_all_https_git_credential_rows_for_test_project(&s)
            .await
            .is_empty()
    );
    s.db.finish().await;
}
