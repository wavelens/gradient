/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    StateApiKey, StateCache, StateConfiguration, StateOrganization, StateProject, StateUpstream,
    StateUser, StateWorker,
};
use crate::types::consts::BASE_ROLE_ADMIN_ID;
use crate::types::input::load_secret_bytes;
use crate::types::*;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use chrono::Utc;
use entity::organization_cache::CacheSubscriptionMode;
use entity::*;
use password_auth::generate_hash;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use ssh_key::PrivateKey;
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

// ── Entry point ───────────────────────────────────────────────────────────────

pub(super) async fn apply_state_to_database(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    crypt_secret_file: &str,
    delete_state: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Applying state to database");

    let app = StateApplicator {
        db,
        crypt_secret_file,
    };

    app.apply_users(&config.users).await?;
    app.apply_organizations(&config.organizations).await?;
    app.apply_projects(&config.projects).await?;
    app.apply_caches(&config.caches).await?;
    app.apply_api_keys(&config.api_keys).await?;
    app.apply_workers(&config.workers).await?;
    app.unmark_removed_entities(config, delete_state).await?;

    println!("State applied successfully");
    tracing::info!("State applied successfully");
    Ok(())
}

// ── StateApplicator ───────────────────────────────────────────────────────────

/// Applies a [`StateConfiguration`] to the database.
///
/// Captures the database connection and crypt secret so each `apply_*` method
/// does not repeat those parameters.
struct StateApplicator<'a> {
    db: &'a DatabaseConnection,
    crypt_secret_file: &'a str,
}

impl<'a> StateApplicator<'a> {
    // ── Lookup helpers ────────────────────────────────────────────────────────

    async fn user_lookup(&self) -> Result<HashMap<String, Uuid>, Box<dyn std::error::Error>> {
        let users = user::Entity::find().all(self.db).await?;
        Ok(users.into_iter().map(|u| (u.username, u.id)).collect())
    }

    async fn org_lookup(&self) -> Result<HashMap<String, Uuid>, Box<dyn std::error::Error>> {
        let orgs = organization::Entity::find().all(self.db).await?;
        Ok(orgs.into_iter().map(|o| (o.name, o.id)).collect())
    }

    // ── apply_users ───────────────────────────────────────────────────────────

    async fn apply_users(
        &self,
        state_users: &HashMap<String, StateUser>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for state_user in state_users.values() {
            let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
                .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
            let password_path = format!(
                "{}/gradient_user_{}_password",
                credentials_dir, state_user.username
            );
            let password = fs::read_to_string(&password_path)
                .map_err(|e| format!("Failed to read password file {}: {}", password_path, e))?;

            let existing_user = user::Entity::find()
                .filter(user::Column::Username.eq(&state_user.username))
                .one(self.db)
                .await?;

            let now = Utc::now().naive_utc();

            if let Some(existing) = existing_user {
                let mut user: user::ActiveModel = existing.into();
                user.name = Set(state_user.name.clone());
                user.email = Set(state_user.email.clone());
                user.password = Set(Some(generate_hash(password.trim())));
                user.email_verified = Set(state_user.email_verified);
                user.superuser = Set(state_user.superuser);
                user.managed = Set(true);
                user.update(self.db).await?;
                tracing::info!("Updated managed user: {}", state_user.username);
            } else {
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
                    superuser: Set(state_user.superuser),
                };
                user.insert(self.db).await?;
                tracing::info!("Created managed user: {}", state_user.username);
            }
        }

        Ok(())
    }

    // ── apply_organizations ───────────────────────────────────────────────────

    async fn apply_organizations(
        &self,
        state_orgs: &HashMap<String, StateOrganization>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_map = self.user_lookup().await?;

        for state_org in state_orgs.values() {
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

            let public_key = derive_public_key(private_key.trim())?;
            let secret = load_secret_bytes(self.crypt_secret_file);

            let encrypted_bytes =
                crypter::encrypt_with_password(secret.expose(), private_key.trim())
                    .ok_or_else(|| "Failed to encrypt SSH private key".to_string())?;
            let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_bytes);

            let created_by_id = user_map
                .get(&state_org.created_by)
                .ok_or_else(|| format!("User '{}' not found", state_org.created_by))?;

            let existing_org = organization::Entity::find()
                .filter(organization::Column::Name.eq(&state_org.name))
                .one(self.db)
                .await?;

            let now = Utc::now().naive_utc();

            let org_id = if let Some(existing) = existing_org {
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
                org.update(self.db).await?;
                tracing::info!("Updated managed organization: {}", state_org.name);
                org_id
            } else {
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
                    github_installation_id: Set(None),
                    forge_webhook_secret: Set(None),
                };
                org.insert(self.db).await?;
                tracing::info!("Created managed organization: {}", state_org.name);
                org_id
            };

            let existing_membership = organization_user::Entity::find()
                .filter(organization_user::Column::Organization.eq(org_id))
                .filter(organization_user::Column::User.eq(*created_by_id))
                .one(self.db)
                .await?;

            if existing_membership.is_none() {
                let membership = organization_user::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    organization: Set(org_id),
                    user: Set(*created_by_id),
                    role: Set(BASE_ROLE_ADMIN_ID),
                };
                membership.insert(self.db).await?;
                tracing::info!(
                    "Added {} as admin member of organization: {}",
                    state_org.created_by,
                    state_org.name
                );
            }
        }

        Ok(())
    }

    // ── apply_projects ────────────────────────────────────────────────────────

    async fn apply_projects(
        &self,
        state_projects: &HashMap<String, StateProject>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_project in state_projects.values() {
            let created_by_id = user_map
                .get(&state_project.created_by)
                .ok_or_else(|| format!("User '{}' not found", state_project.created_by))?;

            let org_id = org_map.get(&state_project.organization).ok_or_else(|| {
                format!("Organization '{}' not found", state_project.organization)
            })?;

            let existing_project = project::Entity::find()
                .filter(project::Column::Name.eq(&state_project.name))
                .one(self.db)
                .await?;

            let now = Utc::now().naive_utc();

            let encrypted_ci_token: Option<Option<String>> = if state_project.ci_reporter_has_token
            {
                let credentials_dir =
                    std::env::var("GRADIENT_CREDENTIALS_DIR").unwrap_or_else(|_| ".".to_string());
                let token_path = format!(
                    "{}/gradient_project_{}_ci_token",
                    credentials_dir, state_project.name
                );
                match fs::read_to_string(&token_path) {
                    Ok(token) => {
                        let secret = load_secret_bytes(self.crypt_secret_file);
                        match crypter::encrypt_with_password(secret.expose(), token.trim()) {
                            Some(encrypted_bytes) => {
                                Some(Some(general_purpose::STANDARD.encode(&encrypted_bytes)))
                            }
                            None => {
                                tracing::warn!(
                                    project = %state_project.name,
                                    "Failed to encrypt CI reporter token, skipping"
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %token_path,
                            project = %state_project.name,
                            "Failed to read CI reporter token credential, skipping"
                        );
                        None
                    }
                }
            } else if state_project.ci_reporter_type.is_some() {
                Some(None)
            } else {
                None
            };

            if let Some(existing) = existing_project {
                let mut proj: project::ActiveModel = existing.into();
                proj.organization = Set(*org_id);
                proj.active = Set(state_project.active);
                proj.display_name = Set(state_project.display_name.clone());
                proj.description = Set(state_project.description.clone());
                proj.repository = Set(state_project.repository.clone());
                proj.evaluation_wildcard = Set(state_project.evaluation_wildcard.clone());
                proj.force_evaluation = Set(state_project.force_evaluation);
                proj.created_by = Set(*created_by_id);
                if let Some(ci_type) = &state_project.ci_reporter_type {
                    proj.ci_reporter_type = Set(Some(ci_type.clone()));
                }
                if let Some(ci_url) = &state_project.ci_reporter_url {
                    proj.ci_reporter_url = Set(Some(ci_url.clone()));
                }
                if let Some(token_opt) = encrypted_ci_token {
                    proj.ci_reporter_token = Set(token_opt);
                }
                proj.managed = Set(true);
                proj.update(self.db).await?;
                tracing::info!("Updated managed project: {}", state_project.name);
            } else {
                let proj = project::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    organization: Set(*org_id),
                    name: Set(state_project.name.clone()),
                    active: Set(state_project.active),
                    display_name: Set(state_project.display_name.clone()),
                    description: Set(state_project.description.clone()),
                    repository: Set(state_project.repository.clone()),
                    evaluation_wildcard: Set(state_project.evaluation_wildcard.clone()),
                    force_evaluation: Set(state_project.force_evaluation),
                    created_by: Set(*created_by_id),
                    ci_reporter_type: Set(state_project.ci_reporter_type.clone()),
                    ci_reporter_url: Set(state_project.ci_reporter_url.clone()),
                    ci_reporter_token: Set(encrypted_ci_token.unwrap_or(None)),
                    last_evaluation: Set(None),
                    last_check_at: Set(now),
                    created_at: Set(now),
                    managed: Set(true),
                    keep_evaluations: Set(0),
                };
                proj.insert(self.db).await?;
                tracing::info!("Created managed project: {}", state_project.name);
            }
        }

        Ok(())
    }

    // ── apply_caches ──────────────────────────────────────────────────────────

    async fn apply_caches(
        &self,
        state_caches: &HashMap<String, StateCache>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_cache in state_caches.values() {
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

            let public_key = general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..]);

            let secret = load_secret_bytes(self.crypt_secret_file);
            let encrypted_bytes =
                crypter::encrypt_with_password(secret.expose(), signing_key.trim()).ok_or_else(
                    || {
                        format!(
                            "Failed to encrypt signing key for cache '{}'",
                            state_cache.name
                        )
                    },
                )?;
            let encrypted_signing_key = general_purpose::STANDARD.encode(&encrypted_bytes);

            let created_by_id = user_map
                .get(&state_cache.created_by)
                .ok_or_else(|| format!("User '{}' not found", state_cache.created_by))?;

            let existing_cache = cache::Entity::find()
                .filter(cache::Column::Name.eq(&state_cache.name))
                .one(self.db)
                .await?;

            let now = Utc::now().naive_utc();

            let cache_id = if let Some(existing) = existing_cache {
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
                cache_model.update(self.db).await?;
                tracing::info!("Updated managed cache: {}", state_cache.name);
                existing.id
            } else {
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
                cache_model.insert(self.db).await?;
                tracing::info!("Created managed cache: {}", state_cache.name);
                cache_id
            };

            self.apply_cache_upstreams(cache_id, &state_cache.name, &state_cache.upstreams)
                .await?;

            for org_name in &state_cache.organizations {
                let org_id = org_map.get(org_name).ok_or_else(|| {
                    format!(
                        "Organization '{}' not found for cache '{}'",
                        org_name, state_cache.name
                    )
                })?;

                let existing_association = organization_cache::Entity::find()
                    .filter(organization_cache::Column::Organization.eq(*org_id))
                    .filter(organization_cache::Column::Cache.eq(cache_id))
                    .one(self.db)
                    .await?;

                if existing_association.is_none() {
                    let org_cache_model = organization_cache::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        organization: Set(*org_id),
                        cache: Set(cache_id),
                        mode: Set(organization_cache::CacheSubscriptionMode::ReadWrite),
                    };
                    org_cache_model.insert(self.db).await?;
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
        &self,
        cache_id: Uuid,
        cache_name: &str,
        upstreams: &[StateUpstream],
    ) -> Result<(), Box<dyn std::error::Error>> {
        ECacheUpstream::delete_many()
            .filter(CCacheUpstream::Cache.eq(cache_id))
            .exec(self.db)
            .await?;

        if upstreams.is_empty() {
            return Ok(());
        }

        let cache_lookup: HashMap<String, Uuid> = ECache::find()
            .all(self.db)
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
            record.insert(self.db).await?;
        }

        tracing::debug!(
            "Applied {} upstreams to cache '{}'",
            upstreams.len(),
            cache_name
        );
        Ok(())
    }

    // ── apply_api_keys ────────────────────────────────────────────────────────

    async fn apply_api_keys(
        &self,
        state_api_keys: &HashMap<String, StateApiKey>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_lookup = self.user_lookup().await?;
        let now = Utc::now().naive_utc();

        for state_api_key in state_api_keys.values() {
            let Some(owned_by_id) = user_lookup.get(&state_api_key.owned_by) else {
                return Err(format!(
                    "User '{}' not found for API key '{}'",
                    state_api_key.owned_by, state_api_key.name
                )
                .into());
            };

            let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
                .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());
            let key_path = format!(
                "{}/gradient_api_{}_key",
                credentials_dir, state_api_key.name
            );
            let key_value = fs::read_to_string(&key_path)
                .map_err(|e| format!("Failed to read API key file {}: {}", key_path, e))?;

            let existing_api_key = api::Entity::find()
                .filter(api::Column::Name.eq(&state_api_key.name))
                .filter(api::Column::OwnedBy.eq(*owned_by_id))
                .one(self.db)
                .await?;

            if let Some(api_key_model) = existing_api_key {
                let mut api_key: api::ActiveModel = api_key_model.into();
                api_key.key = Set(key_value.trim().to_string());
                api_key.managed = Set(true);
                api_key.update(self.db).await?;
                tracing::info!("Updated managed API key: {}", state_api_key.name);
            } else {
                let api_key_model = api::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    owned_by: Set(*owned_by_id),
                    name: Set(state_api_key.name.clone()),
                    key: Set(key_value.trim().to_string()),
                    last_used_at: Set(now),
                    created_at: Set(now),
                    managed: Set(true),
                };
                api_key_model.insert(self.db).await?;
                tracing::info!("Created managed API key: {}", state_api_key.name);
            }
        }

        Ok(())
    }

    // ── apply_workers ─────────────────────────────────────────────────────────

    async fn apply_workers(
        &self,
        state_workers: &HashMap<String, StateWorker>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sha2::{Digest, Sha256};

        let org_map = self.org_lookup().await?;

        let credentials_dir = std::env::var("GRADIENT_CREDENTIALS_DIR")
            .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string());

        for state_worker in state_workers.values() {
            let token_path = format!(
                "{}/gradient_worker_{}_token",
                credentials_dir, state_worker.worker_id
            );
            let token = fs::read_to_string(&token_path)
                .map_err(|e| format!("Failed to read worker token file {}: {}", token_path, e))?;
            let token = token.trim();

            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            let token_hash = hex::encode(hasher.finalize());

            let peer_id = *org_map
                .get(&state_worker.organization)
                .ok_or_else(|| format!("Organization '{}' not found", state_worker.organization))?;

            let existing = worker_registration::Entity::find()
                .filter(worker_registration::Column::PeerId.eq(peer_id))
                .filter(worker_registration::Column::WorkerId.eq(&state_worker.worker_id))
                .one(self.db)
                .await?;

            let now = chrono::Utc::now().naive_utc();

            let url = if state_worker.url.is_empty() {
                None
            } else {
                Some(state_worker.url.clone())
            };

            if let Some(existing) = existing {
                let mut reg: worker_registration::ActiveModel = existing.into();
                reg.token_hash = Set(token_hash);
                reg.managed = Set(true);
                reg.url = Set(url);
                reg.name = Set(state_worker.name.clone());
                reg.update(self.db).await?;
                tracing::info!("Updated worker registration: {}", state_worker.worker_id);
            } else {
                let reg = worker_registration::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    peer_id: Set(peer_id),
                    worker_id: Set(state_worker.worker_id.clone()),
                    token_hash: Set(token_hash),
                    managed: Set(true),
                    url: Set(url),
                    name: Set(state_worker.name.clone()),
                    active: Set(true),
                    created_at: Set(now),
                };
                reg.insert(self.db).await?;
                tracing::info!("Created worker registration: {}", state_worker.worker_id);
            }
        }

        Ok(())
    }

    // ── unmark_removed_entities ───────────────────────────────────────────────

    async fn unmark_removed_entities(
        &self,
        config: &StateConfiguration,
        delete_state: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state_usernames: std::collections::HashSet<&String> = config.users.keys().collect();
        let state_org_names: std::collections::HashSet<&String> =
            config.organizations.keys().collect();
        let state_project_names: std::collections::HashSet<&String> =
            config.projects.keys().collect();
        let state_cache_names: std::collections::HashSet<&String> = config.caches.keys().collect();
        let state_api_key_names: std::collections::HashSet<&String> =
            config.api_keys.keys().collect();
        let state_worker_ids: std::collections::HashSet<&String> =
            config.workers.values().map(|w| &w.worker_id).collect();

        let db = self.db;

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

        let managed_workers = worker_registration::Entity::find()
            .filter(worker_registration::Column::Managed.eq(true))
            .all(db)
            .await?;
        for reg in managed_workers {
            if !state_worker_ids.contains(&reg.worker_id) {
                let worker_id = reg.worker_id.clone();
                worker_registration::Entity::delete_by_id(reg.id)
                    .exec(db)
                    .await?;
                tracing::info!("Deleted worker registration: {}", worker_id);
            }
        }

        Ok(())
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

fn derive_public_key(private_key: &str) -> Result<String> {
    let private_key =
        PrivateKey::from_openssh(private_key).context("Failed to parse private key")?;

    let public_key = private_key
        .public_key()
        .to_openssh()
        .context("Failed to derive public key")?;

    let key_parts: Vec<&str> = public_key.split_whitespace().collect();
    let cleaned_key = if key_parts.len() >= 2 {
        format!("{} {}", key_parts[0], key_parts[1])
    } else {
        public_key.to_string()
    };

    Ok(cleaned_key)
}
