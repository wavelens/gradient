/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::{api, ids::*};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::{all_statements, mock_db};
use gradient_fixtures::rows::user;
use gradient_test_support::web::make_test_server;
use gradient_types::permissions::{Permission, mask_from};
use sea_orm::{DatabaseConnection, MockExecResult};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn pinned_project() -> ProjectId {
    ProjectId::new(uuid::uuid!("01a1bd7c-40fa-72be-896a-75a00d8bf9fd"))
}

fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([("num_items", sea_orm::Value::BigInt(Some(num)))])
}

fn connection_with_project_pinned_key(raw: &str) -> DatabaseConnection {
    let now = chrono::Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "pinned".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(Permission::ALL),
        project: Some(pinned_project()),
        ..Default::default()
    };
    mock_db()
        .append_query_results([vec![key.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![key]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![count_row(0)]])
        .append_query_results([Vec::<gradient_entity::project::Model>::new()])
        .append_query_results([Vec::<gradient_entity::cache::Model>::new()])
        .into_connection()
}

async fn listing_is_limited_to_the_pinned_project(path: &str, table: &str, raw: &str) {
    let conn = connection_with_project_pinned_key(raw);
    let server = make_test_server(conn.clone());

    let res = server
        .get(path)
        .add_header("authorization", format!("Bearer GRAD{raw}"))
        .await;

    res.assert_status_ok();
    drop(server);
    let pinned = pinned_project().to_string();
    let listings: Vec<String> = all_statements(conn.into_transaction_log())
        .into_iter()
        .map(|s| s.to_string())
        .filter(|sql| sql.starts_with(&format!("SELECT \"{table}\"")))
        .collect();
    assert!(!listings.is_empty(), "{path} lists {table} rows");
    assert!(
        listings.iter().all(|sql| sql.contains(&pinned)),
        "{path} filters on the pinned project: {listings:?}"
    );
}

#[tokio::test]
async fn a_project_pinned_key_lists_only_its_project() {
    listing_is_limited_to_the_pinned_project("/api/v1/projects", "project", &"p".repeat(64)).await;
}

#[tokio::test]
async fn a_project_pinned_key_lists_only_the_caches_of_its_project() {
    listing_is_limited_to_the_pinned_project("/api/v1/caches", "cache", &"c".repeat(64)).await;
}
