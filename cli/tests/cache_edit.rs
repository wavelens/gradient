/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `gradient cache edit` must PATCH the named cache. It used to PUT
//! `/caches`, which creates, and it prefilled the editor with a hardcoded
//! max storage of 0 instead of the cache's own, so an unrelated edit made
//! the cache unlimited.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn home_with(server: &MockServer) -> TempDir {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();
    home
}

async fn prod_cache(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/prod"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": {
                "id": "cache-1",
                "name": "prod",
                "display_name": "Prod",
                "description": "the production cache",
                "priority": 40,
                "local_priority": null,
                "max_storage_gb": 100,
                "active": true,
                "managed": false,
                "created_by": "user-1",
                "created_at": "2026-09-18T00:00:00"
            }
        })))
        .mount(server)
        .await;
}

/// `EDITOR=true` leaves the prefilled buffer untouched, so the request body is
/// exactly what the command put in front of the user.
fn edit(home: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("EDITOR", "true")
        .args(args)
        .assert()
}

#[tokio::test]
async fn an_untouched_edit_patches_the_cache_with_its_own_values() {
    let server = MockServer::start().await;
    prod_cache(&server).await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/prod"))
        .and(body_json(serde_json::json!({
            "name": null,
            "display_name": "Prod",
            "description": "the production cache",
            "priority": 40,
            "max_storage_gb": 100
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"error": false, "message": "updated"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    edit(&home, &["cache", "edit", "prod"]).success();
}

#[tokio::test]
async fn a_flag_overrides_one_field_and_leaves_the_rest() {
    let server = MockServer::start().await;
    prod_cache(&server).await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/prod"))
        .and(body_json(serde_json::json!({
            "name": null,
            "display_name": "Production",
            "description": "the production cache",
            "priority": 40,
            "max_storage_gb": 100
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"error": false, "message": "updated"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let home = home_with(&server);
    edit(
        &home,
        &["cache", "edit", "prod", "--display-name", "Production"],
    )
    .success();
}
