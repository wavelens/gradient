/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;
pub mod export;
mod provisioning;
mod validation;

pub use config::*;
pub use export::export_state;
pub use provisioning::{
    PendingProjectMembership, PendingProjectMemberships, StateApplyResult,
    apply_pending_project_memberships,
};
pub use validation::{ValidationError, ValidationResult};

use sea_orm::DatabaseConnection;

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
            pending: PendingProjectMemberships::new(),
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
