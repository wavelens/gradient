/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /api/v1/config` exposes the `create_org` / `create_cache` permission
//! knobs so the frontend can hide the create buttons (issue #470).

use gradient_test_support::web::{make_test_server, make_test_server_configured};
use gradient_types::CreatePermission;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn config_defaults_to_everyone() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres);
        let server = make_test_server(db.into_connection());

        let res = server.get("/api/v1/config").await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"]["create_org"], "everyone");
        assert_eq!(body["message"]["create_cache"], "everyone");
    });
}

#[test]
fn config_reflects_configured_permissions() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres);
        let server = make_test_server_configured(db.into_connection(), |cli| {
            cli.server.create_org = CreatePermission::None;
            cli.server.create_cache = CreatePermission::Superusers;
        });

        let res = server.get("/api/v1/config").await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"]["create_org"], "none");
        assert_eq!(body["message"]["create_cache"], "superusers");
    });
}
