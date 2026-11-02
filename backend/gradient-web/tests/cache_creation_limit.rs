/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_core::CacheCreationLimit;
use gradient_entity::user;
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct FixedAnswer(bool);

#[async_trait::async_trait]
impl CacheCreationLimit for FixedAnswer {
    async fn allows_another_cache(&self, _user: &user::Model) -> anyhow::Result<bool> {
        Ok(self.0)
    }
}

fn signed_in_server(allows_another_cache: bool) -> (TestServer, String) {
    let (db, auth) = signed_in();
    let server = TestState::new()
        .web_db(db.into_connection())
        .cache_creation_limit(Arc::new(FixedAnswer(allows_another_cache)))
        .server();
    (server, auth)
}

#[tokio::test]
async fn a_user_at_the_limit_cannot_create_a_cache() {
    let (server, auth) = signed_in_server(false);

    let res = server
        .put("/api/v1/caches")
        .add_header("authorization", auth)
        .json(&json!({
            "name": "acme", "display_name": "Acme", "description": "",
            "priority": 40, "public": false,
        }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    let body: Value = res.json();
    assert_eq!(body["code"], "cache_limit_reached");
}

#[tokio::test]
async fn the_current_user_reports_whether_another_cache_fits() {
    for allows in [true, false] {
        let (server, auth) = signed_in_server(allows);

        let res = server
            .get("/api/v1/user")
            .add_header("authorization", auth)
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"]["can_create_cache"], allows);
    }
}
