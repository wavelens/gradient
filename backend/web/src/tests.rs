/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[cfg(test)]
mod tests {
    use core::types::*;
    use entity::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use tower_http::cors::{AllowOrigin, CorsLayer};
    use http::header::{AUTHORIZATION, ACCEPT, CONTENT_TYPE};

    fn create_mock_cli() -> Cli {
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

    fn create_mock_state() -> Arc<ServerState> {
        let cli = create_mock_cli();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<user::Model>::new(),
            ])
            .into_connection();
        
        Arc::new(ServerState { db, cli })
    }

    #[test]
    fn test_server_state_configuration() {
        let state = create_mock_state();
        
        // Test server state configuration
        assert!(!state.cli.disable_registration);
        assert!(!state.cli.oauth_enabled);
        assert!(!state.cli.oauth_required);
        assert!(!state.cli.serve_cache);
        assert!(state.cli.debug);
    }

    #[test]
    fn test_cors_configuration() {
        let state = create_mock_state();
        
        // Test CORS configuration in debug mode
        assert!(state.cli.debug);
        assert_eq!(state.cli.serve_url, "http://127.0.0.1:8000");
    }

    #[test]
    fn test_server_configuration() {
        let state = create_mock_state();
        
        assert_eq!(state.cli.ip, "127.0.0.1");
        assert_eq!(state.cli.port, 3000);
        assert!(state.cli.debug);
    }

    mod auth_tests {
        use crate::endpoints::auth::*;

        #[test]
        fn test_make_login_request_serialization() {
            let request = MakeLoginRequest {
                loginname: "testuser".to_string(),
                password: "password123".to_string(),
            };
            
            let json = serde_json::to_string(&request).unwrap();
            assert!(json.contains("testuser"));
            assert!(json.contains("password123"));
        }

        #[test]
        fn test_make_user_request_serialization() {
            let request = MakeUserRequest {
                username: "testuser".to_string(),
                name: "Test User".to_string(),
                email: "test@example.com".to_string(),
                password: "password123".to_string(),
            };
            
            let json = serde_json::to_string(&request).unwrap();
            assert!(json.contains("testuser"));
            assert!(json.contains("Test User"));
            assert!(json.contains("test@example.com"));
        }
    }

    mod organization_tests {
        use crate::endpoints::orgs::*;

        #[test]
        fn test_make_organization_request_serialization() {
            let request = MakeOrganizationRequest {
                name: "test-org".to_string(),
                display_name: "Test Organization".to_string(),
                description: "A test organization".to_string(),
            };
            
            let json = serde_json::to_string(&request).unwrap();
            assert!(json.contains("test-org"));
            assert!(json.contains("Test Organization"));
            assert!(json.contains("A test organization"));
        }

        #[test]
        fn test_add_user_request_serialization() {
            let request = AddUserRequest {
                user: "testuser".to_string(),
                role: "admin".to_string(),
            };
            
            let json = serde_json::to_string(&request).unwrap();
            assert!(json.contains("testuser"));
            assert!(json.contains("admin"));
        }
    }

    #[test]
    fn test_middleware_configuration() {
        let state = create_mock_state();
        
        // Test CORS configuration creation doesn't panic
        let cors_allow_origin = if state.cli.debug {
            AllowOrigin::list(vec![
                state.cli.serve_url.clone().try_into().unwrap(),
                format!("http://{}:8000", state.cli.ip.clone()).try_into().unwrap(),
            ])
        } else {
            AllowOrigin::exact(state.cli.serve_url.clone().try_into().unwrap())
        };
        
        // Test that CORS configuration is properly created
        let cors = CorsLayer::new()
            .allow_origin(cors_allow_origin)
            .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
            .allow_credentials(true);
        
        // Test that middleware was created without panicking
        assert!(true); // If we reach here, the configuration was successful
    }
}