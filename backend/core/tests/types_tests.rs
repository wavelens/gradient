/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for types and data structures

extern crate core as gradient_core;
use gradient_core::types::*;
use sea_orm::{DatabaseBackend, MockDatabase};

fn create_mock_cli() -> Cli {
    Cli {
        debug: false,
        log_level: "info".to_string(),
        ip: "127.0.0.1".to_string(),
        port: 3000,
        serve_url: "http://127.0.0.1:8000".to_string(),
        database_url: Some("mock://test".to_string()),
        database_url_file: None,
        max_concurrent_evaluations: 10,
        max_concurrent_builds: 1000,
        evaluation_timeout: 10,
        store_path: None,
        state_file: None,
        base_path: ".".to_string(),
        disable_registration: false,
        oidc_enabled: false,
        oidc_required: false,
        oidc_client_id: None,
        oidc_client_secret_file: None,
        oidc_scopes: None,
        oidc_discovery_url: None,
        crypt_secret_file: "test_secret".to_string(),
        jwt_secret_file: "test_jwt".to_string(),
        serve_cache: false,
        binpath_nix: "nix".to_string(),
        binpath_git: "git".to_string(),
        binpath_ssh: "ssh".to_string(),
        report_errors: false,
        email_enabled: false,
        email_require_verification: false,
        email_smtp_host: None,
        email_smtp_port: 587,
        email_smtp_username: None,
        email_smtp_password_file: None,
        email_from_address: None,
        email_from_name: "Gradient".to_string(),
    }
}

fn create_mock_db() -> sea_orm::DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<entity::organization::Model>::new()])
        .into_connection()
}

#[test]
fn test_server_state_creation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let cli = create_mock_cli();
        let db = create_mock_db();

        let state = ServerState { db, cli };

        assert_eq!(state.cli.port, 3000);
        assert_eq!(state.cli.ip, "127.0.0.1");
    });
}

#[test]
fn test_nix_cache_info_to_string() {
    let cache_info = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: 40,
    };

    let expected = "WantMassQuery: true\nStoreDir: /nix/store\nPriority: 40";
    assert_eq!(cache_info.to_nix_string(), expected);
}

#[test]
fn test_nix_path_info_to_string() {
    let path_info = NixPathInfo {
        store_path: "/nix/store/test-path".to_string(),
        url: "nar/test.nar.xz".to_string(),
        compression: "xz".to_string(),
        file_hash: "sha256:abcd1234".to_string(),
        file_size: 1024,
        nar_hash: "sha256:efgh5678".to_string(),
        nar_size: 2048,
        references: vec!["/nix/store/ref1".to_string(), "/nix/store/ref2".to_string()],
        sig: "cache.example.com:signature".to_string(),
        ca: Some("fixed:sha256:xyz".to_string()),
    };

    let result = path_info.to_nix_string();
    assert!(result.contains("StorePath: /nix/store/test-path"));
    assert!(result.contains("URL: nar/test.nar.xz"));
    assert!(result.contains("Compression: xz"));
    assert!(result.contains("FileHash: sha256:abcd1234"));
    assert!(result.contains("FileSize: 1024"));
    assert!(result.contains("NarHash: sha256:efgh5678"));
    assert!(result.contains("NarSize: 2048"));
    assert!(result.contains("References: /nix/store/ref1 /nix/store/ref2"));
    assert!(result.contains("Sig: cache.example.com:signature"));
    assert!(result.contains("CA: fixed:sha256:xyz"));
}

#[test]
fn test_nix_path_info_without_ca() {
    let path_info = NixPathInfo {
        store_path: "/nix/store/test-path".to_string(),
        url: "nar/test.nar.xz".to_string(),
        compression: "xz".to_string(),
        file_hash: "sha256:abcd1234".to_string(),
        file_size: 1024,
        nar_hash: "sha256:efgh5678".to_string(),
        nar_size: 2048,
        references: vec![],
        sig: "cache.example.com:signature".to_string(),
        ca: None,
    };

    let result = path_info.to_nix_string();
    assert!(!result.contains("CA:"));
    assert!(result.contains("References: "));
}

#[test]
fn test_build_output_path_deserialization() {
    let json = r#"{
        "id": "test-id",
        "outPath": "/nix/store/test-path",
        "signatures": ["signature1", "signature2"]
    }"#;

    let output_path: BuildOutputPath = serde_json::from_str(json).unwrap();
    assert_eq!(output_path.id, "test-id");
    assert_eq!(output_path.out_path, "/nix/store/test-path");
    assert_eq!(output_path.signatures.len(), 2);
    assert_eq!(output_path.signatures[0], "signature1");
    assert_eq!(output_path.signatures[1], "signature2");
}
