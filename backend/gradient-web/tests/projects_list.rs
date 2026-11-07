/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::RoleId;
use gradient_entity::role;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows::{project, project_member, user, view_role};
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::permissions::{Permission, mask_from};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([("num_items", sea_orm::Value::BigInt(Some(num)))])
}

async fn list_as_member_with(role: role::Model) -> Value {
    let (db, auth) = signed_in_as(user());
    let db = db
        .append_query_results([vec![count_row(1)]])
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .append_query_results([vec![project_member(role.id)]])
        .append_query_results([vec![role]]);

    let res = make_test_server(db.into_connection())
        .get("/api/v1/projects")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    res.json::<Value>()["message"]["items"][0].clone()
}

#[tokio::test]
async fn the_list_names_the_permissions_of_a_custom_role() {
    let subscriber = role::Model {
        id: RoleId::new(Uuid::from_u128(0x5b)),
        name: "subscriber".into(),
        project: Some(project_id()),
        permission: mask_from(&[Permission::ViewProject, Permission::ManageSubscriptions]),
        managed: false,
    };

    let item = list_as_member_with(subscriber).await;

    assert_eq!(item["role"], "subscriber");
    assert_eq!(
        item["permissions"],
        json!(["viewProject", "manageSubscriptions"])
    );
}

#[tokio::test]
async fn a_view_member_sees_only_the_view_permission_in_the_list() {
    let item = list_as_member_with(view_role()).await;

    assert_eq!(item["role"], "View");
    assert_eq!(item["permissions"], json!(["viewProject"]));
}
