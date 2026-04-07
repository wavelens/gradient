/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::consts::BASE_ROLE_ADMIN_ID;
use crate::input::load_secret_bytes;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use chrono::Utc;
use entity::organization_cache::CacheSubscriptionMode;
use entity::*;
use password_auth::generate_hash;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use ssh_key::PrivateKey;
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

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

        // Validate users
        for user in self.users.values() {
            if !user.email.contains('@') {
                errors.push(ValidationError {
                    field: format!("users.{}.email", user.username),
                    message: "Invalid email format".to_string(),
                });
            }
        }

        // Validate organizations
        for org in self.organizations.values() {
            if !self.users.contains_key(&org.created_by) {
                errors.push(ValidationError {
                    field: format!("organizations.{}.created_by", org.name),
                    message: format!("User '{}' does not exist", org.created_by),
                });
            }
        }

        // Validate projects
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

        // Validate servers
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

        // Validate caches
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

        // Validate API keys
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

    // Validate configuration
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

    // TODO: Apply state to database
    // This will be implemented in the next step
    apply_state_to_database(db, &config, crypt_secret_file, delete_state).await?;

    Ok(())
}

async fn apply_state_to_database(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    crypt_secret_file: &str,
    delete_state: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Applying state to database");

    // Apply users
    apply_users(db, &config.users).await?;

    // Apply organizations (depends on users)
    apply_organizations(db, &config.organizations, &config.users, crypt_secret_file).await?;

    // Apply projects (depends on organizations and users)
    apply_projects(db, &config.projects, &config.users, &config.organizations).await?;

    // Apply servers (depends on organizations and users)
    apply_servers(db, &config.servers, &config.users, &config.organizations).await?;

    // Apply caches (depends on users and organizations)
    apply_caches(
        db,
        &config.caches,
        &config.users,
        &config.organizations,
        crypt_secret_file,
    )
    .await?;

    // Apply API keys (depends on users)
    apply_api_keys(db, &config.api_keys).await?;

    // Unmark entities that are no longer in state
    unmark_removed_entities(db, config, delete_state).await?;

    println!("State applied successfully");
    tracing::info!("State applied successfully");
    Ok(())
}

async fn apply_users(
    db: &DatabaseConnection,
    state_users: &HashMap<String, StateUser>,
) -> Result<(), Box<dyn std::error::Error>> {
    for state_user in state_users.values() {
        // Read password from file using credentials directory
        let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
        let password_path = format!(
            "{}/gradient_user_{}_password",
            credentials_dir, state_user.username
        );
        let password = fs::read_to_string(&password_path)
            .map_err(|e| format!("Failed to read password file {}: {}", password_path, e))?;

        // Check if user exists
        let existing_user = user::Entity::find()
            .filter(user::Column::Username.eq(&state_user.username))
            .one(db)
            .await?;

        let now = Utc::now().naive_utc();

        if let Some(existing) = existing_user {
            // Update existing user
            let mut user: user::ActiveModel = existing.into();
            user.name = Set(state_user.name.clone());
            user.email = Set(state_user.email.clone());
            user.password = Set(Some(generate_hash(password.trim())));
            user.email_verified = Set(state_user.email_verified);
            user.managed = Set(true);
            user.update(db).await?;
            tracing::info!("Updated managed user: {}", state_user.username);
        } else {
            // Create new user
            let user = user::ActiveModel {
                id: Set(Uuid::new_v4()),
                username: Set(state_user.username.clone()),
                name: Set(state_user.name.clone()),
                email: Set(state_user.email.clone()),
                password: Set(Some(generate_hash(password.trim()))),
                last_login_at: Set(now),
                created_at: Set(now),
                email_verified: Set(state_user.email_verified),
                email_verification_token: Set(None),
                email_verification_token_expires: Set(None),
                managed: Set(true),
            };
            user.insert(db).await?;
            tracing::info!("Created managed user: {}", state_user.username);
        }
    }

    Ok(())
}

fn derive_public_key(private_key: &str) -> Result<String> {
    let private_key =
        PrivateKey::from_openssh(private_key).context("Failed to parse private key")?;

    let public_key = private_key
        .public_key()
        .to_openssh()
        .context("Failed to derive public key")?;

    // Remove default comment if present (only keep algorithm and key)
    let key_parts: Vec<&str> = public_key.split_whitespace().collect();
    let cleaned_key = if key_parts.len() >= 2 {
        format!("{} {}", key_parts[0], key_parts[1])
    } else {
        public_key.to_string()
    };

    Ok(cleaned_key)
}

async fn apply_organizations(
    db: &DatabaseConnection,
    state_orgs: &HashMap<String, StateOrganization>,
    _state_users: &HashMap<String, StateUser>,
    crypt_secret_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_map = create_user_lookup(db).await?;

    for state_org in state_orgs.values() {
        // Read private key from file using credentials directory
        let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
        let private_key_path = format!(
            "{}/gradient_org_{}_private_key",
            credentials_dir, state_org.name
        );
        let private_key = fs::read_to_string(&private_key_path).map_err(|e| {
            format!(
                "Failed to read private key file {}: {}",
                private_key_path, e
            )
        })?;

        // Derive the actual public key from the private key
        let public_key = derive_public_key(private_key.trim())?;

        // Encrypt private key using crypter library
        let secret = load_secret_bytes(crypt_secret_file);

        let encrypted_bytes = crypter::encrypt_with_password(&secret, private_key.trim())
            .ok_or_else(|| "Failed to encrypt SSH private key".to_string())?;
        let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_bytes);

        let created_by_id = user_map
            .get(&state_org.created_by)
            .ok_or_else(|| format!("User '{}' not found", state_org.created_by))?;

        let existing_org = organization::Entity::find()
            .filter(organization::Column::Name.eq(&state_org.name))
            .one(db)
            .await?;

        let now = Utc::now().naive_utc();

        let org_id = if let Some(existing) = existing_org {
            // Update existing organization
            let org_id = existing.id;
            let mut org: organization::ActiveModel = existing.into();
            org.display_name = Set(state_org.display_name.clone());
            org.description = Set(state_org.description.clone());
            org.public_key = Set(public_key);
            org.private_key = Set(encrypted_private_key.clone());
            org.use_nix_store = Set(state_org.use_nix_store);
            org.created_by = Set(*created_by_id);
            org.public = Set(state_org.public);
            org.managed = Set(true);
            org.update(db).await?;
            tracing::info!("Updated managed organization: {}", state_org.name);
            org_id
        } else {
            // Create new organization
            let org_id = Uuid::new_v4();
            let org = organization::ActiveModel {
                id: Set(org_id),
                name: Set(state_org.name.clone()),
                display_name: Set(state_org.display_name.clone()),
                description: Set(state_org.description.clone()),
                public_key: Set(public_key),
                private_key: Set(encrypted_private_key),
                use_nix_store: Set(state_org.use_nix_store),
                public: Set(state_org.public),
                created_by: Set(*created_by_id),
                created_at: Set(now),
                managed: Set(true),
            };
            org.insert(db).await?;
            tracing::info!("Created managed organization: {}", state_org.name);
            org_id
        };

        // Ensure the created_by user is a member of the organization with admin role
        let existing_membership = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(org_id))
            .filter(organization_user::Column::User.eq(*created_by_id))
            .one(db)
            .await?;

        if existing_membership.is_none() {
            let membership = organization_user::ActiveModel {
                id: Set(Uuid::new_v4()),
                organization: Set(org_id),
                user: Set(*created_by_id),
                role: Set(BASE_ROLE_ADMIN_ID),
            };
            membership.insert(db).await?;
            tracing::info!(
                "Added {} as admin member of organization: {}",
                state_org.created_by,
                state_org.name
            );
        }
    }

    Ok(())
}

async fn apply_projects(
    db: &DatabaseConnection,
    state_projects: &HashMap<String, StateProject>,
    _state_users: &HashMap<String, StateUser>,
    _state_orgs: &HashMap<String, StateOrganization>,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_map = create_user_lookup(db).await?;
    let org_map = create_org_lookup(db).await?;

    for state_project in state_projects.values() {
        let created_by_id = user_map
            .get(&state_project.created_by)
            .ok_or_else(|| format!("User '{}' not found", state_project.created_by))?;

        let org_id = org_map
            .get(&state_project.organization)
            .ok_or_else(|| format!("Organization '{}' not found", state_project.organization))?;

        let existing_project = project::Entity::find()
            .filter(project::Column::Name.eq(&state_project.name))
            .one(db)
            .await?;

        let now = Utc::now().naive_utc();

        if let Some(existing) = existing_project {
            // Update existing project
            let mut proj: project::ActiveModel = existing.into();
            proj.organization = Set(*org_id);
            proj.active = Set(state_project.active);
            proj.display_name = Set(state_project.display_name.clone());
            proj.description = Set(state_project.description.clone());
            proj.repository = Set(state_project.repository.clone());
            proj.evaluation_wildcard = Set(state_project.evaluation_wildcard.clone());
            proj.force_evaluation = Set(state_project.force_evaluation);
            proj.created_by = Set(*created_by_id);
            proj.managed = Set(true);
            if let Some(keep) = state_project.keep_evaluations {
                proj.keep_evaluations = Set(keep);
            }
            proj.update(db).await?;
            tracing::info!("Updated managed project: {}", state_project.name);
        } else {
            // Create new project
            let proj = project::ActiveModel {
                id: Set(Uuid::new_v4()),
                organization: Set(*org_id),
                name: Set(state_project.name.clone()),
                active: Set(state_project.active),
                display_name: Set(state_project.display_name.clone()),
                description: Set(state_project.description.clone()),
                repository: Set(state_project.repository.clone()),
                evaluation_wildcard: Set(state_project.evaluation_wildcard.clone()),
                last_evaluation: Set(None),
                last_check_at: Set(now),
                force_evaluation: Set(state_project.force_evaluation),
                created_by: Set(*created_by_id),
                created_at: Set(now),
                managed: Set(true),
                keep_evaluations: Set(state_project.keep_evaluations.unwrap_or(30)),
            };
            proj.insert(db).await?;
            tracing::info!("Created managed project: {}", state_project.name);
        }
    }

    Ok(())
}

async fn apply_servers(
    db: &DatabaseConnection,
    state_servers: &HashMap<String, StateServer>,
    _state_users: &HashMap<String, StateUser>,
    _state_orgs: &HashMap<String, StateOrganization>,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_map = create_user_lookup(db).await?;
    let org_map = create_org_lookup(db).await?;

    for state_server in state_servers.values() {
        let created_by_id = user_map
            .get(&state_server.created_by)
            .ok_or_else(|| format!("User '{}' not found", state_server.created_by))?;

        let org_id = org_map
            .get(&state_server.organization)
            .ok_or_else(|| format!("Organization '{}' not found", state_server.organization))?;

        let existing_server = server::Entity::find()
            .filter(server::Column::Name.eq(&state_server.name))
            .one(db)
            .await?;

        let now = Utc::now().naive_utc();

        if let Some(existing) = existing_server {
            // Update existing server
            let mut serv: server::ActiveModel = existing.into();
            serv.display_name = Set(state_server.display_name.clone());
            serv.organization = Set(*org_id);
            serv.active = Set(state_server.active);
            serv.host = Set(state_server.host.clone());
            serv.port = Set(state_server.port);
            serv.username = Set(state_server.username.clone());
            serv.max_concurrent_builds = Set(state_server.max_concurrent_builds);
            serv.created_by = Set(*created_by_id);
            serv.managed = Set(true);
            serv.update(db).await?;
            tracing::info!("Updated managed server: {}", state_server.name);
        } else {
            // Create new server
            let serv = server::ActiveModel {
                id: Set(Uuid::new_v4()),
                name: Set(state_server.name.clone()),
                display_name: Set(state_server.display_name.clone()),
                organization: Set(*org_id),
                active: Set(state_server.active),
                host: Set(state_server.host.clone()),
                port: Set(state_server.port),
                username: Set(state_server.username.clone()),
                last_connection_at: Set(now),
                max_concurrent_builds: Set(state_server.max_concurrent_builds),
                created_by: Set(*created_by_id),
                created_at: Set(now),
                managed: Set(true),
            };
            serv.insert(db).await?;
            tracing::info!("Created managed server: {}", state_server.name);
        }

        // Handle server features and architectures
        apply_server_features(db, &state_server.name, &state_server.features).await?;
        apply_server_architectures(db, &state_server.name, &state_server.architectures).await?;
    }

    Ok(())
}

async fn apply_caches(
    db: &DatabaseConnection,
    state_caches: &HashMap<String, StateCache>,
    _state_users: &HashMap<String, StateUser>,
    _state_orgs: &HashMap<String, StateOrganization>,
    crypt_secret_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_map = create_user_lookup(db).await?;
    let org_map = create_org_lookup(db).await?;

    for state_cache in state_caches.values() {
        // Read signing key from file using credentials directory
        let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
        let signing_key_path = format!(
            "{}/gradient_cache_{}_signing_key",
            credentials_dir, state_cache.name
        );
        let signing_key = fs::read_to_string(&signing_key_path).map_err(|e| {
            format!(
                "Failed to read signing key file {}: {}",
                signing_key_path, e
            )
        })?;

        // Validate that the signing key is base64 encoded and derive the public key
        let key_bytes = general_purpose::STANDARD
            .decode(signing_key.trim())
            .map_err(|e| {
                format!(
                    "Signing key for cache '{}' is not a valid base64 encoded string: {}",
                    state_cache.name, e
                )
            })?;

        if key_bytes.len() < 32 {
            return Err(format!(
                "Signing key for cache '{}' is too short (expected at least 32 bytes)",
                state_cache.name
            )
            .into());
        }

        // The last 32 bytes of the ed25519 keypair are the public key
        let public_key = general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..]);

        // Encrypt private key using crypter library
        let secret = load_secret_bytes(crypt_secret_file);

        let encrypted_bytes = crypter::encrypt_with_password(&secret, signing_key.trim())
            .ok_or_else(|| {
                format!(
                    "Failed to encrypt signing key for cache '{}'",
                    state_cache.name
                )
            })?;
        let encrypted_signing_key = general_purpose::STANDARD.encode(&encrypted_bytes);

        let created_by_id = user_map
            .get(&state_cache.created_by)
            .ok_or_else(|| format!("User '{}' not found", state_cache.created_by))?;

        let existing_cache = cache::Entity::find()
            .filter(cache::Column::Name.eq(&state_cache.name))
            .one(db)
            .await?;

        let now = Utc::now().naive_utc();

        let cache_id = if let Some(existing) = existing_cache {
            // Update existing cache
            let mut cache_model: cache::ActiveModel = existing.clone().into();
            cache_model.display_name = Set(state_cache.display_name.clone());
            cache_model.description = Set(state_cache.description.clone());
            cache_model.active = Set(state_cache.active);
            cache_model.priority = Set(state_cache.priority);
            cache_model.public_key = Set(public_key.clone());
            cache_model.private_key = Set(encrypted_signing_key.clone());
            cache_model.created_by = Set(*created_by_id);
            cache_model.public = Set(state_cache.public);
            cache_model.managed = Set(true);
            cache_model.update(db).await?;
            tracing::info!("Updated managed cache: {}", state_cache.name);
            existing.id
        } else {
            // Create new cache
            let cache_id = Uuid::new_v4();
            let cache_model = cache::ActiveModel {
                id: Set(cache_id),
                name: Set(state_cache.name.clone()),
                display_name: Set(state_cache.display_name.clone()),
                description: Set(state_cache.description.clone()),
                active: Set(state_cache.active),
                priority: Set(state_cache.priority),
                public_key: Set(public_key),
                private_key: Set(encrypted_signing_key),
                public: Set(state_cache.public),
                created_by: Set(*created_by_id),
                created_at: Set(now),
                managed: Set(true),
            };
            cache_model.insert(db).await?;
            tracing::info!("Created managed cache: {}", state_cache.name);
            cache_id
        };

        // Apply upstream caches
        apply_cache_upstreams(db, cache_id, &state_cache.name, &state_cache.upstreams).await?;

        // Create organization_cache associations
        for org_name in &state_cache.organizations {
            let org_id = org_map.get(org_name).ok_or_else(|| {
                format!(
                    "Organization '{}' not found for cache '{}'",
                    org_name, state_cache.name
                )
            })?;

            // Check if association already exists
            let existing_association = organization_cache::Entity::find()
                .filter(organization_cache::Column::Organization.eq(*org_id))
                .filter(organization_cache::Column::Cache.eq(cache_id))
                .one(db)
                .await?;

            if existing_association.is_none() {
                let org_cache_model = organization_cache::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    organization: Set(*org_id),
                    cache: Set(cache_id),
                    mode: Set(organization_cache::CacheSubscriptionMode::ReadWrite),
                };
                org_cache_model.insert(db).await?;
                tracing::info!(
                    "Created organization_cache association: {} -> {}",
                    org_name,
                    state_cache.name
                );
            }
        }
    }

    Ok(())
}

async fn apply_cache_upstreams(
    db: &DatabaseConnection,
    cache_id: Uuid,
    cache_name: &str,
    upstreams: &[StateUpstream],
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::types::*;
    use sea_orm::*;

    // Replace all upstreams for this cache with the state-declared ones.
    ECacheUpstream::delete_many()
        .filter(CCacheUpstream::Cache.eq(cache_id))
        .exec(db)
        .await?;

    if upstreams.is_empty() {
        return Ok(());
    }

    // Build a name→id lookup for internal upstream resolution.
    let cache_lookup: HashMap<String, Uuid> = ECache::find()
        .all(db)
        .await?
        .into_iter()
        .map(|c| (c.name, c.id))
        .collect();

    for upstream in upstreams {
        let record = match upstream {
            StateUpstream::Internal {
                cache_name: upstream_cache_name,
                display_name,
                mode,
            } => {
                let upstream_id = *cache_lookup.get(upstream_cache_name).ok_or_else(|| {
                    format!(
                        "Cache '{}' not found for upstream of cache '{}'",
                        upstream_cache_name, cache_name
                    )
                })?;
                let name = display_name
                    .clone()
                    .unwrap_or_else(|| upstream_cache_name.clone());
                ACacheUpstream {
                    id: Set(Uuid::new_v4()),
                    cache: Set(cache_id),
                    display_name: Set(name),
                    mode: Set(mode.clone()),
                    upstream_cache: Set(Some(upstream_id)),
                    url: Set(None),
                    public_key: Set(None),
                }
            }
            StateUpstream::External {
                display_name,
                url,
                public_key,
            } => ACacheUpstream {
                id: Set(Uuid::new_v4()),
                cache: Set(cache_id),
                display_name: Set(display_name.clone()),
                mode: Set(CacheSubscriptionMode::ReadOnly),
                upstream_cache: Set(None),
                url: Set(Some(url.clone())),
                public_key: Set(Some(public_key.clone())),
            },
        };
        record.insert(db).await?;
    }

    tracing::debug!(
        "Applied {} upstreams to cache '{}'",
        upstreams.len(),
        cache_name
    );
    Ok(())
}

async fn apply_api_keys(
    db: &DatabaseConnection,
    state_api_keys: &HashMap<String, StateApiKey>,
) -> Result<(), Box<dyn std::error::Error>> {
    let user_lookup = create_user_lookup(db).await?;
    let now = Utc::now().naive_utc();

    for state_api_key in state_api_keys.values() {
        let Some(owned_by_id) = user_lookup.get(&state_api_key.owned_by) else {
            return Err(format!(
                "User '{}' not found for API key '{}'",
                state_api_key.owned_by, state_api_key.name
            )
            .into());
        };

        // Read API key from file using credentials directory
        let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
        let key_path = format!(
            "{}/gradient_api_{}_key",
            credentials_dir, state_api_key.name
        );
        let key_value = fs::read_to_string(&key_path)
            .map_err(|e| format!("Failed to read API key file {}: {}", key_path, e))?;

        // Check if API key exists
        let existing_api_key = api::Entity::find()
            .filter(api::Column::Name.eq(&state_api_key.name))
            .filter(api::Column::OwnedBy.eq(*owned_by_id))
            .one(db)
            .await?;

        if let Some(api_key_model) = existing_api_key {
            // Update existing API key
            let mut api_key: api::ActiveModel = api_key_model.into();
            api_key.key = Set(key_value.trim().to_string());
            api_key.managed = Set(true);
            api_key.update(db).await?;
            tracing::info!("Updated managed API key: {}", state_api_key.name);
        } else {
            // Create new API key
            let api_key_model = api::ActiveModel {
                id: Set(Uuid::new_v4()),
                owned_by: Set(*owned_by_id),
                name: Set(state_api_key.name.clone()),
                key: Set(key_value.trim().to_string()),
                last_used_at: Set(now),
                created_at: Set(now),
                managed: Set(true),
            };
            api_key_model.insert(db).await?;
            tracing::info!("Created managed API key: {}", state_api_key.name);
        }
    }

    Ok(())
}

async fn apply_server_features(
    db: &DatabaseConnection,
    server_name: &str,
    features: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::types::*;
    use sea_orm::*;

    // Find the server by name
    let server = EServer::find()
        .filter(CServer::Name.eq(server_name))
        .one(db)
        .await?;

    let Some(server) = server else {
        return Err(format!("Server '{}' not found", server_name).into());
    };

    // Delete existing server features
    EServerFeature::delete_many()
        .filter(CServerFeature::Server.eq(server.id))
        .exec(db)
        .await?;

    // Add new features
    for feature_name in features {
        // Find or create the feature
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(feature_name))
            .one(db)
            .await?;

        let feature = if let Some(feature) = feature {
            feature
        } else {
            let afeature = AFeature {
                id: Set(Uuid::new_v4()),
                name: Set(feature_name.clone()),
            };
            afeature.insert(db).await?
        };

        // Create server-feature association
        let aserver_feature = AServerFeature {
            id: Set(Uuid::new_v4()),
            server: Set(server.id),
            feature: Set(feature.id),
        };

        aserver_feature.insert(db).await?;
    }

    tracing::debug!(
        "Applied {} features to server '{}'",
        features.len(),
        server_name
    );
    Ok(())
}

async fn apply_server_architectures(
    db: &DatabaseConnection,
    server_name: &str,
    architectures: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::types::*;
    use sea_orm::*;

    // Find the server by name
    let server = EServer::find()
        .filter(CServer::Name.eq(server_name))
        .one(db)
        .await?;

    let Some(server) = server else {
        return Err(format!("Server '{}' not found", server_name).into());
    };

    // Delete existing server architectures
    EServerArchitecture::delete_many()
        .filter(CServerArchitecture::Server.eq(server.id))
        .exec(db)
        .await?;

    // Parse and validate architectures
    let parsed_architectures: Result<Vec<server::Architecture>, _> =
        architectures.iter().map(|arch| arch.parse()).collect();

    let parsed_architectures =
        parsed_architectures.map_err(|_| "Invalid architecture specified")?;

    if parsed_architectures.is_empty() {
        return Err("No valid architectures specified".into());
    }

    // Create server architecture associations
    let server_architecture_models: Vec<AServerArchitecture> = parsed_architectures
        .into_iter()
        .map(|arch| AServerArchitecture {
            id: Set(Uuid::new_v4()),
            server: Set(server.id),
            architecture: Set(arch),
        })
        .collect();

    EServerArchitecture::insert_many(server_architecture_models)
        .exec(db)
        .await?;

    tracing::debug!(
        "Applied {} architectures to server '{}'",
        architectures.len(),
        server_name
    );
    Ok(())
}

async fn unmark_removed_entities(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    delete_state: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create sets of managed entity names from config
    let state_usernames: std::collections::HashSet<&String> = config.users.keys().collect();
    let state_org_names: std::collections::HashSet<&String> = config.organizations.keys().collect();
    let state_project_names: std::collections::HashSet<&String> = config.projects.keys().collect();
    let state_server_names: std::collections::HashSet<&String> = config.servers.keys().collect();
    let state_cache_names: std::collections::HashSet<&String> = config.caches.keys().collect();
    let state_api_key_names: std::collections::HashSet<&String> = config.api_keys.keys().collect();

    // Unmark users not in state
    let managed_users = user::Entity::find()
        .filter(user::Column::Managed.eq(true))
        .all(db)
        .await?;

    for user_model in managed_users {
        if !state_usernames.contains(&user_model.username) {
            let username = user_model.username.clone();
            if delete_state {
                user::Entity::delete_by_id(user_model.id).exec(db).await?;
                tracing::info!("Deleted user: {}", username);
            } else {
                let mut user: user::ActiveModel = user_model.into();
                user.managed = Set(false);
                user.update(db).await?;
                tracing::info!("Unmanaged user: {}", username);
            }
        }
    }

    // Unmark organizations not in state
    let managed_orgs = organization::Entity::find()
        .filter(organization::Column::Managed.eq(true))
        .all(db)
        .await?;

    for org_model in managed_orgs {
        if !state_org_names.contains(&org_model.name) {
            let org_name = org_model.name.clone();
            if delete_state {
                organization::Entity::delete_by_id(org_model.id)
                    .exec(db)
                    .await?;
                tracing::info!("Deleted organization: {}", org_name);
            } else {
                let mut org: organization::ActiveModel = org_model.into();
                org.managed = Set(false);
                org.update(db).await?;
                tracing::info!("Unmanaged organization: {}", org_name);
            }
        }
    }

    // Unmark projects not in state
    let managed_projects = project::Entity::find()
        .filter(project::Column::Managed.eq(true))
        .all(db)
        .await?;

    for project_model in managed_projects {
        if !state_project_names.contains(&project_model.name) {
            let project_name = project_model.name.clone();
            if delete_state {
                project::Entity::delete_by_id(project_model.id)
                    .exec(db)
                    .await?;
                tracing::info!("Deleted project: {}", project_name);
            } else {
                let mut project: project::ActiveModel = project_model.into();
                project.managed = Set(false);
                project.update(db).await?;
                tracing::info!("Unmanaged project: {}", project_name);
            }
        }
    }

    // Unmark servers not in state
    let managed_servers = server::Entity::find()
        .filter(server::Column::Managed.eq(true))
        .all(db)
        .await?;

    for server_model in managed_servers {
        if !state_server_names.contains(&server_model.name) {
            let server_name = server_model.name.clone();
            if delete_state {
                server::Entity::delete_by_id(server_model.id)
                    .exec(db)
                    .await?;
                tracing::info!("Deleted server: {}", server_name);
            } else {
                let mut server: server::ActiveModel = server_model.into();
                server.managed = Set(false);
                server.update(db).await?;
                tracing::info!("Unmanaged server: {}", server_name);
            }
        }
    }

    // Unmark caches not in state
    let managed_caches = cache::Entity::find()
        .filter(cache::Column::Managed.eq(true))
        .all(db)
        .await?;

    for cache_model in managed_caches {
        if !state_cache_names.contains(&cache_model.name) {
            let cache_name = cache_model.name.clone();
            if delete_state {
                cache::Entity::delete_by_id(cache_model.id).exec(db).await?;
                tracing::info!("Deleted cache: {}", cache_name);
            } else {
                let mut cache: cache::ActiveModel = cache_model.into();
                cache.managed = Set(false);
                cache.update(db).await?;
                tracing::info!("Unmanaged cache: {}", cache_name);
            }
        }
    }

    // Unmark API keys not in state
    let managed_api_keys = api::Entity::find()
        .filter(api::Column::Managed.eq(true))
        .all(db)
        .await?;

    for api_key_model in managed_api_keys {
        if !state_api_key_names.contains(&api_key_model.name) {
            let api_key_name = api_key_model.name.clone();
            if delete_state {
                api::Entity::delete_by_id(api_key_model.id).exec(db).await?;
                tracing::info!("Deleted API key: {}", api_key_name);
            } else {
                let mut api_key: api::ActiveModel = api_key_model.into();
                api_key.managed = Set(false);
                api_key.update(db).await?;
                tracing::info!("Unmanaged API key: {}", api_key_name);
            }
        }
    }

    Ok(())
}

async fn create_user_lookup(
    db: &DatabaseConnection,
) -> Result<HashMap<String, Uuid>, Box<dyn std::error::Error>> {
    let users = user::Entity::find().all(db).await?;
    Ok(users.into_iter().map(|u| (u.username, u.id)).collect())
}

async fn create_org_lookup(
    db: &DatabaseConnection,
) -> Result<HashMap<String, Uuid>, Box<dyn std::error::Error>> {
    let orgs = organization::Entity::find().all(db).await?;
    Ok(orgs.into_iter().map(|o| (o.name, o.id)).collect())
}
