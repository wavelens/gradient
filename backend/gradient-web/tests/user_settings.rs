/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::UserId;
use gradient_entity::{user, user_identity};
use gradient_fixtures::mock::all_statements;
use gradient_fixtures::rows::user;
use gradient_test_support::web::{make_test_server, signed_in};
use serde_json::json;

const SETTINGS: &str = "/api/v1/user/settings";

fn other_user() -> user::Model {
    user::Model {
        id: UserId::now_v7(),
        username: "someone-else".into(),
        email: "someone-else@example.com".into(),
        ..user()
    }
}

#[tokio::test]
async fn saving_the_profile_with_the_own_username_and_email_succeeds() {
    let (db, auth) = signed_in();
    let renamed = user::Model {
        name: "Renamed".into(),
        ..user()
    };
    let db = db
        .append_query_results([Vec::<user_identity::Model>::new()])
        .append_query_results([vec![user()]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![renamed]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch(SETTINGS)
        .add_header("authorization", auth)
        .json(&json!({
            "username": user().username,
            "name": "Renamed",
            "email": user().email,
        }))
        .await;

    res.assert_status_ok();
}

#[tokio::test]
async fn taking_the_username_of_another_user_is_refused() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([Vec::<user_identity::Model>::new()])
        .append_query_results([vec![other_user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch(SETTINGS)
        .add_header("authorization", auth)
        .json(&json!({ "username": other_user().username }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn taking_the_email_of_another_user_is_refused() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([Vec::<user_identity::Model>::new()])
        .append_query_results([vec![other_user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch(SETTINGS)
        .add_header("authorization", auth)
        .json(&json!({ "email": other_user().email }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_email_clash_check_ignores_case() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([Vec::<user_identity::Model>::new()])
        .append_query_results([vec![other_user()]])
        .into_connection();
    let server = make_test_server(db.clone());

    let res = server
        .patch(SETTINGS)
        .add_header("authorization", auth)
        .json(&json!({ "email": other_user().email.to_uppercase() }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
    drop(server);
    let lookup = all_statements(db.into_transaction_log())
        .into_iter()
        .rev()
        .find(|s| s.sql.contains(r#""user"."email""#))
        .expect("email lookup");
    assert!(
        lookup.sql.to_lowercase().contains("lower("),
        "{}",
        lookup.sql
    );
}

#[tokio::test]
async fn an_invalid_email_is_refused() {
    let (db, auth) = signed_in();
    let db = db.append_query_results([Vec::<user_identity::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch(SETTINGS)
        .add_header("authorization", auth)
        .json(&json!({ "email": "not an address" }))
        .await;

    res.assert_status_bad_request();
}

#[tokio::test]
async fn an_empty_user_search_returns_no_users_without_a_query() {
    let (db, auth) = signed_in();
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/user/search?q=%20")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: serde_json::Value = res.json();
    assert_eq!(body["message"], json!([]));
}

#[tokio::test]
async fn an_api_key_with_a_blank_name_is_refused() {
    let (db, auth) = signed_in();
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/user/keys")
        .add_header("authorization", auth)
        .json(&json!({ "name": "  ", "permissions": ["viewProject"] }))
        .await;

    res.assert_status_bad_request();
}
