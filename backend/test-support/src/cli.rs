/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::Cli;

/// Single source of truth for the `Cli` struct in tests.
/// Update only here when fields are added/removed from `Cli`.
pub fn test_cli() -> Cli {
    Cli {
        log_level: "error".into(),
        builder_log_level: None,
        cache_log_level: None,
        web_log_level: None,
        ip: "127.0.0.1".into(),
        port: 3000,
        serve_url: "http://127.0.0.1:3000".into(),
        database_url: None,
        database_url_file: None,
        max_concurrent_evaluations: 2,
        max_concurrent_builds: 10,
        evaluation_timeout: 5,
        store_path: None,
        base_path: "/tmp/gradient-test".into(),
        enable_registration: false,
        oidc_enabled: false,
        oidc_required: false,
        oidc_client_id: None,
        oidc_client_secret_file: None,
        oidc_scopes: None,
        oidc_discovery_url: None,
        crypt_secret_file: "test-secret".into(),
        jwt_secret_file: "test-jwt".into(),
        serve_cache: false,
        binpath_nix: "nix".into(),
        binpath_ssh: "ssh".into(),
        report_errors: false,
        email_enabled: false,
        email_require_verification: false,
        email_smtp_host: None,
        email_smtp_port: 587,
        email_smtp_username: None,
        email_smtp_password_file: None,
        email_from_address: None,
        email_from_name: "Gradient Test".into(),
        email_enable_tls: false,
        state_file: None,
        delete_state: true,
        keep_evaluations: 30,
        max_nixdaemon_connections: 2,
        eval_workers: 1,
        nar_ttl_hours: 0,
    }
}
