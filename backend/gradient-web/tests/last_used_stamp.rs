/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Two requests with one API key against one server write `last_used_at`
//! once: the second request stages no UPDATE result and still succeeds.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::{api, ids::*};
use gradient_test_support::fixtures::{user, user_id};
use gradient_test_support::web::make_test_server;
use sea_orm::{DatabaseBackend, MockDatabase};
use sha2::{Digest, Sha256};

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

fn api_key(raw: &str) -> api::Model {
    let now = chrono::Utc::now().naive_utc();

    api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "ci".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission: gradient_db::permissions::admin_mask(),
        ..Default::default()
    }
}

#[tokio::test]
async fn one_api_key_is_stamped_once_per_interval() {
    let raw = "s".repeat(64);
    let key = api_key(&raw);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![user()]])
        .into_connection();
    let server = make_test_server(db);

    for _ in 0..2 {
        server
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer GRAD{raw}"))
            .await
            .assert_status_ok();
    }
}
