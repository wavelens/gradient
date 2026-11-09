/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_fixtures::rows::{admin_role, project, project_member, user, view_role};
use gradient_test_support::web::make_test_server;
use gradient_test_support::web::signed_in;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use sea_orm::MockDatabase;
use serde_json::json;
use std::collections::BTreeMap;

const MEMBERS: &str = "/api/v1/projects/test-project/users";

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

fn sole_admin_targets_self() -> (MockDatabase, String) {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]]);
    (db, auth)
}

#[tokio::test]
async fn update_member_blocks_demoting_last_admin() {
    let (db, auth) = sole_admin_targets_self();
    let db = db
        .append_query_results([vec![view_role()]])
        .append_query_results([vec![count_row(1)]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .patch(MEMBERS)
        .add_header("authorization", auth)
        .json(&json!({"user": "testuser", "role": "View"}))
        .await;

    res.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        res.json::<serde_json::Value>()["message"],
        "Cannot remove the last Admin from the Project."
    );
}

#[tokio::test]
async fn remove_member_blocks_last_admin() {
    let (db, auth) = sole_admin_targets_self();
    let db = db.append_query_results([vec![count_row(1)]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .delete(MEMBERS)
        .add_header("authorization", auth)
        .json(&json!({"user": "testuser"}))
        .await;

    res.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        res.json::<serde_json::Value>()["message"],
        "Cannot remove the last Admin from the Project."
    );
}
