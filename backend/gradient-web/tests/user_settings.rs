/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::UserId;
use gradient_entity::{user, user_identity};
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
