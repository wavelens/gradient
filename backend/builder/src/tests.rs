/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[cfg(test)]
mod tests {
    use crate::start_builder;
    use core::types::*;
    use entity::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use uuid::Uuid;
    use chrono::NaiveDate;

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
            .append_query_results([
                Vec::<organization::Model>::new(),
            ])
            .into_connection();
        
        Arc::new(ServerState { db, cli })
    }

    #[tokio::test]
    async fn test_start_builder() {
        let state = create_mock_state();
        
        let result = start_builder(state).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_builder_state_configuration() {
        let state = create_mock_state();
        
        assert_eq!(state.cli.max_concurrent_evaluations, 10);
        assert_eq!(state.cli.max_concurrent_builds, 1000);
        assert_eq!(state.cli.evaluation_timeout, 10);
        assert_eq!(state.cli.binpath_nix, "nix");
    }

    #[test]
    fn test_build_status_enum() {
        use entity::build::BuildStatus;
        
        // Test enum values exist and can be compared
        let created = BuildStatus::Created;
        let queued = BuildStatus::Queued;
        let building = BuildStatus::Building;
        let completed = BuildStatus::Completed;
        let failed = BuildStatus::Failed;
        let aborted = BuildStatus::Aborted;
        
        // Test that all enum variants are different
        assert_ne!(created, queued);
        assert_ne!(queued, building);
        assert_ne!(building, completed);
        assert_ne!(completed, failed);
        assert_ne!(failed, aborted);
    }

    #[test]
    fn test_evaluation_status_enum() {
        use entity::evaluation::EvaluationStatus;
        
        // Test enum values exist and can be compared
        let queued = EvaluationStatus::Queued;
        let evaluating = EvaluationStatus::Evaluating;
        let building = EvaluationStatus::Building;
        let completed = EvaluationStatus::Completed;
        let failed = EvaluationStatus::Failed;
        let aborted = EvaluationStatus::Aborted;
        
        // Test that all enum variants are different
        assert_ne!(queued, evaluating);
        assert_ne!(evaluating, building);
        assert_ne!(building, completed);
        assert_ne!(completed, failed);
        assert_ne!(failed, aborted);
    }

    #[test]
    fn test_concurrent_limits() {
        let cli = create_mock_cli();
        
        // Test that concurrent limits are properly configured
        assert!(cli.max_concurrent_evaluations > 0);
        assert!(cli.max_concurrent_builds > 0);
        assert!(cli.max_concurrent_evaluations <= cli.max_concurrent_builds);
        assert_eq!(cli.evaluation_timeout, 10);
    }

    #[test]
    fn test_binary_paths() {
        let cli = create_mock_cli();
        
        // Test that binary paths are configured
        assert_eq!(cli.binpath_nix, "nix");
        assert_eq!(cli.binpath_git, "git");
        assert_eq!(cli.binpath_zstd, "zstd");
    }
}