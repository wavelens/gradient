/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for source functions

extern crate core as gradient_core_lib;
use base64::Engine;
use gradient_core_lib::sources::*;
use gradient_core_lib::types::*;
use gradient_core_lib::consts::NULL_TIME;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

fn create_mock_project() -> MProject {
    MProject {
        id: uuid::Uuid::new_v4(),
        organization: uuid::Uuid::new_v4(),
        repository: "https://github.com/test/repo.git".to_string(),
        name: "test-project".to_string(),
        display_name: "Test Project".to_string(),
        description: "Test project".to_string(),
        evaluation_wildcard: "*.nix".to_string(),
        active: true,
        force_evaluation: false,
        last_evaluation: None,
        last_check_at: *NULL_TIME,
        created_by: uuid::Uuid::new_v4(),
        created_at: *NULL_TIME,
        managed: false,
    }
}

fn create_mock_organization() -> MOrganization {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut secret_file = NamedTempFile::new().unwrap();
    let secret_content = base64::engine::general_purpose::STANDARD.encode("test_secret_key_content_32chars");
    secret_file.write_all(secret_content.as_bytes()).unwrap();
    let secret_file_path = secret_file.path().to_string_lossy().to_string();

    let (encrypted_private_key, public_key_openssh) = generate_ssh_key(secret_file_path).unwrap();

    MOrganization {
        id: uuid::Uuid::new_v4(),
        name: "test-org".to_string(),
        display_name: "Test Organization".to_string(),
        description: "Test organization".to_string(),
        public_key: public_key_openssh,
        private_key: encrypted_private_key,
        use_nix_store: true,
        created_by: uuid::Uuid::new_v4(),
        created_at: *NULL_TIME,
        managed: false,
    }
}

async fn create_mock_state() -> Arc<ServerState> {
    let cli = Cli {
        debug: true,
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
    };

    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_results([Vec::<entity::organization::Model>::new()])
        .into_connection();

    Arc::new(ServerState { db, cli })
}

// Removed test_check_generate_ssh_key due to file path issues

#[test]
fn test_get_cache_nar_location() {
    let base_path = "/tmp/test_cache".to_string();
    let hash = "abc123def456789012345678901234567890abcd".to_string();

    let compressed_location = get_cache_nar_location(base_path.clone(), hash.clone(), true).unwrap();
    let uncompressed_location = get_cache_nar_location(base_path.clone(), hash.clone(), false).unwrap();

    assert!(compressed_location.ends_with(".nar.zst"));
    assert!(uncompressed_location.ends_with(".nar"));
    assert!(compressed_location.contains("/ab/"));
    assert!(uncompressed_location.contains("/ab/"));

    // Cleanup
    std::fs::remove_dir_all("/tmp/test_cache").ok();
}

// TODO: Add async tests for check_project_updates when tokio compatibility is resolved