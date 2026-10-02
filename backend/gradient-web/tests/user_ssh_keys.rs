/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_test_support::fixtures::user;
use gradient_test_support::web::{live_session, make_test_server_configured, make_token};
use gradient_types::{MUserSshKey, SessionId, UserId, UserSshKeyId};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{Value, json};

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";
const KEY_FINGERPRINT: &str = "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo";

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

fn stored_key(owner: UserId) -> MUserSshKey {
    MUserSshKey {
        id: UserSshKeyId::now_v7(),
        user: owner,
        name: "laptop".into(),
        public_key: KEY.into(),
        fingerprint: KEY_FINGERPRINT.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn adding_a_key_stores_its_fingerprint() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([Vec::<MUserSshKey>::new()])
        .append_query_results([vec![stored_key(user().id)]])
        .into_connection();
    let server = make_test_server_configured(db.clone(), |cli| cli.ssh.enable = true);

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", bearer(session_id))
        .json(&json!({ "name": "laptop", "public_key": KEY }))
        .await;

    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>()["message"]["fingerprint"],
        KEY_FINGERPRINT
    );
    let log = format!("{:?}", db.into_transaction_log());
    assert!(log.contains(KEY_FINGERPRINT), "{log}");
}

#[tokio::test]
async fn an_unparsable_key_is_rejected() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id).into_connection();
    let server = make_test_server_configured(db, |cli| cli.ssh.enable = true);

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", bearer(session_id))
        .json(&json!({ "name": "laptop", "public_key": "ssh-ed25519 garbage" }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_known_fingerprint_is_a_conflict() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![stored_key(UserId::now_v7())]])
        .into_connection();
    let server = make_test_server_configured(db, |cli| cli.ssh.enable = true);

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", bearer(session_id))
        .json(&json!({ "name": "laptop", "public_key": KEY }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn another_users_key_is_not_found() {
    let session_id = SessionId::now_v7();
    let foreign = stored_key(UserId::now_v7());
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
        .append_query_results([vec![foreign.clone()]])
        .into_connection();
    let server = make_test_server_configured(db.clone(), |cli| cli.ssh.enable = true);

    let res = server
        .delete(&format!("/api/v1/user/ssh-keys/{}", foreign.id))
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
    let log = format!("{:?}", db.into_transaction_log());
    assert!(!log.contains("DELETE"), "{log}");
}

#[tokio::test]
async fn the_endpoints_are_missing_when_ssh_is_disabled() {
    let session_id = SessionId::now_v7();
    let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id).into_connection();
    let server = make_test_server_configured(db, |_| {});

    let res = server
        .get("/api/v1/user/ssh-keys")
        .add_header("Authorization", bearer(session_id))
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}
