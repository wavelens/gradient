/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

extern crate core as gradient_core;

use builder::start_builder;
use entity::organization;
use gradient_core::types::{Cli, ServerState};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use tokio;

fn create_mock_cli() -> Cli {
    Cli {
        ip: "127.0.0.1".to_string(),
        port: 3000,
        serve_url: "http://127.0.0.1:8000".to_string(),
        database_url: Some("mock://test".to_string()),
        database_url_file: None,
        max_concurrent_evaluations: 10,
        max_concurrent_builds: 1000,
        evaluation_timeout: 10,
        store_path: None,
        base_path: ".".to_string(),
        disable_registration: false,
        oauth_enabled: false,
        oauth_required: false,
        oauth_client_id: None,
        oauth_client_secret_file: None,
        oauth_auth_url: None,
        oauth_token_url: None,
        oauth_api_url: None,
        oauth_scopes: None,
        oidc_discovery_url: None,
        crypt_secret_file: "test_secret".to_string(),
        jwt_secret_file: "test_jwt".to_string(),
        serve_cache: false,
        binpath_nix: "nix".to_string(),
        binpath_git: "git".to_string(),
        binpath_zstd: "zstd".to_string(),
        report_errors: false,
    }
}

fn create_mock_state() -> Arc<ServerState> {
    let cli = create_mock_cli();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<organization::Model>::new()])
        .into_connection();

    Arc::new(ServerState { db, cli })
}

#[test]
fn test_start_builder() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let state = create_mock_state();

        let result = start_builder(state).await;
        assert!(result.is_ok());
    });
}
