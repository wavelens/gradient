/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_fixtures::rows::user;
use gradient_test_support::state::TestState;
use gradient_test_support::web::make_test_server;
use gradient_test_support::web::signed_in;
use gradient_types::{MUserSshKey, UserId, UserSshKeyId};
use serde_json::{Value, json};

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";
const KEY_FINGERPRINT: &str = "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo";

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
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([Vec::<MUserSshKey>::new()])
        .append_query_results([vec![stored_key(user().id)]])
        .into_connection();
    let server = TestState::new()
        .web_db(db.clone())
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
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
    let (db, auth) = signed_in();
    let db = db.into_connection();
    let server = TestState::new()
        .web_db(db)
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
        .json(&json!({ "name": "laptop", "public_key": "ssh-ed25519 garbage" }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_known_fingerprint_is_a_conflict() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![stored_key(UserId::now_v7())]])
        .into_connection();
    let server = TestState::new()
        .web_db(db)
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
        .json(&json!({ "name": "laptop", "public_key": KEY }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn another_users_key_is_not_found() {
    let (db, auth) = signed_in();
    let foreign = stored_key(UserId::now_v7());
    let db = db
        .append_query_results([vec![foreign.clone()]])
        .into_connection();
    let server = TestState::new()
        .web_db(db.clone())
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .delete(&format!("/api/v1/user/ssh-keys/{}", foreign.id))
        .add_header("Authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
    let log = format!("{:?}", db.into_transaction_log());
    assert!(!log.contains("DELETE"), "{log}");
}

#[tokio::test]
async fn the_endpoints_are_missing_when_ssh_is_disabled() {
    let (db, auth) = signed_in();
    let db = db.into_connection();
    let server = make_test_server(db);

    let res = server
        .get("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}
