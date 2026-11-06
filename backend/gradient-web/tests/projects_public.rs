/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::public_project;
use gradient_test_support::web::make_test_server;
use serde_json::Value;
use std::collections::BTreeMap;

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([("num_items", sea_orm::Value::BigInt(Some(num)))])
}

#[tokio::test]
async fn the_public_listing_leaves_out_the_private_key() {
    let db = mock_db()
        .append_query_results([vec![count_row(1)]])
        .append_query_results([vec![public_project()]]);

    let res = make_test_server(db.into_connection())
        .get("/api/v1/projects/public")
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let item = &body["message"]["items"][0];
    assert_eq!(item["name"], public_project().name);
    assert!(item.get("private_key").is_none(), "{item}");
}
