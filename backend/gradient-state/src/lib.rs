/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Declarative state management: the on-disk DTOs ([`config`]), pre-apply
//! [`validation`], database [`provisioning`], and [`export`]. This root keeps
//! the load entry point and the OIDC/SCIM group → role resolution.

mod config;
pub mod export;
mod provisioning;
mod validation;

pub use config::*;
pub use export::export_state;
pub use provisioning::{
    PendingOrgMembership, PendingOrgMemberships, StateApplyResult, apply_pending_org_memberships,
};
pub use validation::{ValidationError, ValidationResult};

use gradient_types::{OrganizationId, RoleId};
use sea_orm::DatabaseConnection;
use std::collections::HashMap;

/// Resolved at startup from [`StateRole::oidc_group`]: OIDC group name → the
/// `(organization, role)` grants a user presenting that group receives on login.
pub type OidcGroupRoles = HashMap<String, Vec<(OrganizationId, RoleId)>>;

/// Build the OIDC group → grants map from declared roles. `role_ids` maps
/// `(organization_name, role_name)` to the provisioned `(OrganizationId, RoleId)`.
pub fn resolve_oidc_group_roles(
    config: &StateConfiguration,
    role_ids: &HashMap<(String, String), (OrganizationId, RoleId)>,
) -> OidcGroupRoles {
    let mut map: OidcGroupRoles = HashMap::new();
    for role in config.roles.values() {
        if role.oidc_group.is_empty() {
            continue;
        }
        let key = (role.organization.clone(), role.name.clone());
        let Some(&grant) = role_ids.get(&key) else {
            tracing::warn!(
                organization = %role.organization,
                role = %role.name,
                "oidc_group references a role that was not provisioned; skipping",
            );
            continue;
        };
        for group in &role.oidc_group {
            map.entry(group.clone()).or_default().push(grant);
        }
    }
    map
}

/// Resolved at startup from [`StateRole::scim_group`]: SCIM group name → the
/// `(organization, role)` grants a member of that SCIM group receives.
pub type ScimGroupRoles = HashMap<String, Vec<(OrganizationId, RoleId)>>;

/// Build the SCIM group → grants map from declared roles. Mirrors
/// [`resolve_oidc_group_roles`].
pub fn resolve_scim_group_roles(
    config: &StateConfiguration,
    role_ids: &HashMap<(String, String), (OrganizationId, RoleId)>,
) -> ScimGroupRoles {
    let mut map: ScimGroupRoles = HashMap::new();
    for role in config.roles.values() {
        if role.scim_group.is_empty() {
            continue;
        }
        let key = (role.organization.clone(), role.name.clone());
        let Some(&grant) = role_ids.get(&key) else {
            tracing::warn!(
                organization = %role.organization,
                role = %role.name,
                "scim_group references a role that was not provisioned; skipping",
            );
            continue;
        };
        for group in &role.scim_group {
            map.entry(group.clone()).or_default().push(grant);
        }
    }
    map
}

/// Load and validate a state file without touching the database. Returns the
/// human-readable validation errors (empty `Vec` = valid). Backs the
/// `--validate-state` CLI flag so config mistakes surface at build/CI time
/// instead of on server start.
pub fn validate_state_file(path: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let config = StateConfiguration::from_file(path)?;
    Ok(config
        .validate()
        .errors
        .into_iter()
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect())
}

pub async fn load_and_apply_state(
    db: &DatabaseConnection,
    state_file_path: Option<&str>,
    crypt_secret_file: &str,
    delete_state: bool,
    email_enabled: bool,
) -> Result<StateApplyResult, Box<dyn std::error::Error>> {
    let Some(path) = state_file_path else {
        tracing::info!("No state file configured, skipping state management");
        return Ok(StateApplyResult {
            pending: PendingOrgMemberships::new(),
            oidc_group_roles: OidcGroupRoles::new(),
            scim_group_roles: ScimGroupRoles::new(),
        });
    };

    tracing::info!(path, "Loading state configuration");

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

    tracing::info!("State configuration validated successfully");

    let result = provisioning::apply_state_to_database(
        db,
        &config,
        crypt_secret_file,
        delete_state,
        email_enabled,
    )
    .await?;

    Ok(result)
}

#[cfg(test)]
mod tests;
