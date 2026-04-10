/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod provisioning;

use entity::organization_cache::CacheSubscriptionMode;
use serde::{Deserialize, Serialize};
use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateUser {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password_file: String,
    #[serde(default)]
    pub email_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateOrganization {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub private_key_file: String,
    pub public: bool,
    #[serde(default = "default_true")]
    pub use_nix_store: bool,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProject {
    pub name: String,
    pub organization: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    #[serde(default = "default_main")]
    pub evaluation_wildcard: String,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub force_evaluation: bool,
    pub created_by: String,
    /// How many evaluations to retain per project. `None` keeps the current DB value (or the
    /// default of 30 for new projects). Must not exceed `GRADIENT_KEEP_EVALUATIONS` if set.
    #[serde(default)]
    pub keep_evaluations: Option<i32>,
    /// CI reporter type: `"gitea"` or `"github"`. `None` disables CI reporting.
    #[serde(default)]
    pub ci_reporter_type: Option<String>,
    /// Base URL for the CI reporter (required for Gitea; defaults to github.com for GitHub).
    #[serde(default)]
    pub ci_reporter_url: Option<String>,
    /// Whether a CI reporter token credential is provided. When true, state management
    /// reads the token from the systemd credential `gradient_project_{name}_ci_token`
    /// (loaded via `LoadCredential`), encrypts it, and stores it in the database.
    #[serde(default)]
    pub ci_reporter_has_token: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateServer {
    pub name: String,
    pub display_name: String,
    pub organization: String,
    #[serde(default = "default_true")]
    pub active: bool,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: i32,
    pub username: String,
    #[serde(default = "default_architectures")]
    pub architectures: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default = "default_max_concurrent_builds")]
    pub max_concurrent_builds: i32,
    pub created_by: String,
}

fn default_max_concurrent_builds() -> i32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCache {
    pub name: String,
    pub display_name: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub signing_key_file: String,
    #[serde(default)]
    pub organizations: Vec<String>,
    #[serde(default)]
    pub upstreams: Vec<StateUpstream>,
    pub public: bool,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StateUpstream {
    Internal {
        cache_name: String,
        display_name: Option<String>,
        #[serde(default = "default_upstream_mode")]
        mode: CacheSubscriptionMode,
    },
    External {
        display_name: String,
        url: String,
        public_key: String,
    },
}

fn default_upstream_mode() -> CacheSubscriptionMode {
    CacheSubscriptionMode::ReadWrite
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateApiKey {
    pub name: String,
    pub key_file: String,
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfiguration {
    #[serde(default)]
    pub users: HashMap<String, StateUser>,
    #[serde(default)]
    pub organizations: HashMap<String, StateOrganization>,
    #[serde(default)]
    pub projects: HashMap<String, StateProject>,
    #[serde(default)]
    pub servers: HashMap<String, StateServer>,
    #[serde(default)]
    pub caches: HashMap<String, StateCache>,
    #[serde(default)]
    pub api_keys: HashMap<String, StateApiKey>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("Validation error in field '{field}': {message}")]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub is_valid: bool,
}

fn default_true() -> bool {
    true
}

fn default_main() -> String {
    "main".to_string()
}

fn default_ssh_port() -> i32 {
    22
}

fn default_architectures() -> Vec<String> {
    vec!["x86_64-linux".to_string()]
}

fn default_priority() -> i32 {
    10
}

impl StateConfiguration {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let config: StateConfiguration = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn validate(&self) -> ValidationResult {
        let mut errors = Vec::new();

        for user in self.users.values() {
            if !user.email.contains('@') {
                errors.push(ValidationError {
                    field: format!("users.{}.email", user.username),
                    message: "Invalid email format".to_string(),
                });
            }
        }

        for org in self.organizations.values() {
            if !self.users.contains_key(&org.created_by) {
                errors.push(ValidationError {
                    field: format!("organizations.{}.created_by", org.name),
                    message: format!("User '{}' does not exist", org.created_by),
                });
            }
        }

        for project in self.projects.values() {
            if !self.organizations.contains_key(&project.organization) {
                errors.push(ValidationError {
                    field: format!("projects.{}.organization", project.name),
                    message: format!("Organization '{}' does not exist", project.organization),
                });
            }

            if !self.users.contains_key(&project.created_by) {
                errors.push(ValidationError {
                    field: format!("projects.{}.created_by", project.name),
                    message: format!("User '{}' does not exist", project.created_by),
                });
            }

            if !project.repository.starts_with("http") && !project.repository.starts_with("git") {
                errors.push(ValidationError {
                    field: format!("projects.{}.repository", project.name),
                    message: "Repository URL must start with http or git".to_string(),
                });
            }
        }

        for server in self.servers.values() {
            if !self.organizations.contains_key(&server.organization) {
                errors.push(ValidationError {
                    field: format!("servers.{}.organization", server.name),
                    message: format!("Organization '{}' does not exist", server.organization),
                });
            }

            if !self.users.contains_key(&server.created_by) {
                errors.push(ValidationError {
                    field: format!("servers.{}.created_by", server.name),
                    message: format!("User '{}' does not exist", server.created_by),
                });
            }

            for arch in &server.architectures {
                if ![
                    "x86_64-linux",
                    "aarch64-linux",
                    "x86_64-darwin",
                    "aarch64-darwin",
                ]
                .contains(&arch.as_str())
                {
                    errors.push(ValidationError {
                        field: format!("servers.{}.architectures", server.name),
                        message: format!("Unknown architecture: {}", arch),
                    });
                }
            }

            if server.port < 1 || server.port > 65535 {
                errors.push(ValidationError {
                    field: format!("servers.{}.port", server.name),
                    message: "Port must be between 1 and 65535".to_string(),
                });
            }
        }

        for cache in self.caches.values() {
            if !self.users.contains_key(&cache.created_by) {
                errors.push(ValidationError {
                    field: format!("caches.{}.created_by", cache.name),
                    message: format!("User '{}' does not exist", cache.created_by),
                });
            }

            for org_name in &cache.organizations {
                if !self.organizations.contains_key(org_name) {
                    errors.push(ValidationError {
                        field: format!("caches.{}.organizations", cache.name),
                        message: format!("Organization '{}' does not exist", org_name),
                    });
                }
            }
        }

        for api_key in self.api_keys.values() {
            if !self.users.contains_key(&api_key.owned_by) {
                errors.push(ValidationError {
                    field: format!("api_keys.{}.owned_by", api_key.name),
                    message: format!("User '{}' does not exist", api_key.owned_by),
                });
            }
        }

        ValidationResult {
            is_valid: errors.is_empty(),
            errors,
        }
    }
}

pub async fn load_and_apply_state(
    db: &DatabaseConnection,
    state_file_path: Option<&str>,
    crypt_secret_file: &str,
    delete_state: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = state_file_path else {
        tracing::info!("No state file configured, skipping state management");
        return Ok(());
    };

    println!("Loading state configuration from: {}", path);
    tracing::info!("Loading state configuration from: {}", path);

    let config = StateConfiguration::from_file(path)?;

    let validation = config.validate();
    if !validation.is_valid {
        let error_messages: Vec<String> = validation
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect();

        return Err(format!(
            "State configuration validation failed:\n{}",
            error_messages.join("\n")
        )
        .into());
    }

    println!("State configuration validated successfully");
    tracing::info!("State configuration validated successfully");

    provisioning::apply_state_to_database(db, &config, crypt_secret_file, delete_state).await?;

    Ok(())
}
