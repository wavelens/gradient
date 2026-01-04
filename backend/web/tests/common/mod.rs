/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use core::types::*;
use entity::*;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;

pub fn create_mock_cli() -> Cli {
    Cli {
        debug: true,
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

pub fn create_mock_state() -> Arc<ServerState> {
    let cli = create_mock_cli();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<user::Model>::new()])
        .into_connection();

    Arc::new(ServerState { db, cli })
}
