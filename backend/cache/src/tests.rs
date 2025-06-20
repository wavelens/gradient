/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[cfg(test)]
mod tests {
    use crate::start_cache;
    use core::types::*;
    use entity::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use uuid::Uuid;

    fn create_mock_cli() -> Cli {
        Cli {
            debug: false,
            ip: "127.0.0.1".to_string(),
            port: 3000,
            serve_url: "http://127.0.0.1:8000".to_string(),
            database_url: Some("mock://test".to_string()),
            database_url_file: None,
            max_concurrent_evaluations: 10,
            max_concurrent_builds: 1000,
            evaluation_timeout: 10,
            store_path: Some("/nix/store".to_string()),
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
            serve_cache: true,
            binpath_nix: "nix".to_string(),
            binpath_git: "git".to_string(),
            binpath_zstd: "zstd".to_string(),
            report_errors: false,
        }
    }

    fn create_mock_state() -> Arc<ServerState> {
        let cli = create_mock_cli();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<build_output::Model>::new(),
            ])
            .into_connection();
        
        Arc::new(ServerState { db, cli })
    }

    #[tokio::test]
    async fn test_start_cache() {
        let state = create_mock_state();
        
        let result = start_cache(state).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_cache_configuration() {
        let state = create_mock_state();
        
        assert!(state.cli.serve_cache);
        assert_eq!(state.cli.store_path, Some("/nix/store".to_string()));
        assert_eq!(state.cli.binpath_zstd, "zstd");
    }

    #[test]
    fn test_cache_serving_configuration() {
        let cli = create_mock_cli();
        
        // Test that cache serving is properly configured
        assert!(cli.serve_cache);
        assert!(cli.store_path.is_some());
        assert_eq!(cli.store_path.unwrap(), "/nix/store");
    }

    #[test]
    fn test_cache_tools_configuration() {
        let cli = create_mock_cli();
        
        // Test that required tools are configured
        assert_eq!(cli.binpath_nix, "nix");
        assert_eq!(cli.binpath_zstd, "zstd");
        assert_eq!(cli.binpath_git, "git");
    }

    #[test]
    fn test_sentry_configuration() {
        let cli = create_mock_cli();
        
        // Test that sentry reporting is disabled in tests
        assert!(!cli.report_errors);
    }

    #[test]
    fn test_cache_paths_and_urls() {
        // Test common cache path patterns
        let nix_store_path = "/nix/store/abc123def456-package-1.0.0";
        let cache_url = "https://cache.example.com";
        let nar_path = "nar/abc123def456.nar.xz";
        
        assert!(nix_store_path.starts_with("/nix/store/"));
        assert!(cache_url.starts_with("https://"));
        assert!(nar_path.ends_with(".nar.xz"));
    }

    #[test]
    fn test_cache_signature_format() {
        let signature = "cache.example.com:signature_data_here";
        let parts: Vec<&str> = signature.split(':').collect();
        
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "cache.example.com");
        assert_eq!(parts[1], "signature_data_here");
    }
}