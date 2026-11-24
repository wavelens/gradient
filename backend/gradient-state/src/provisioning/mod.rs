/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod credentials;
mod entities;
mod lookups;
mod reconciliation;

use crate::config::StateConfiguration;
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use std::path::Path;

pub(crate) use credentials::{
    derive_public_key, parse_api_key_hash, parse_password_phc, read_credential,
};
pub(crate) use entities::build_trigger_config;
pub(crate) use lookups::{
    StateFileManagedUserIdsByUsername, choose_row_to_update_from_declared_id_and_name,
    load_git_host_connections_usable_by_project_by_name, lookup_id,
    order_state_entries_with_declared_id_first, parse_optional_uuid_string_as_typed_id,
};

pub(crate) type DynError = Box<dyn std::error::Error>;

pub(super) async fn apply_state_to_database(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    crypt_secret_file: &str,
    credentials_dir: &Path,
    delete_state: bool,
    email_enabled: bool,
    user_url_destination_allowlist: &gradient_util::http_validation::UserUrlDestinationAllowlist,
) -> Result<(), DynError> {
    tracing::info!("Applying state to database");

    let transaction = db.begin().await?;
    let app = StateApplicator {
        db: &transaction,
        crypt_secret_file,
        credentials_dir,
        email_enabled,
        user_url_destination_allowlist,
    };

    app.apply_users(&config.users).await?;
    // Teams must exist before projects, for their new-project grants (the local worker's `server`).
    let team_ids = app.apply_teams(&config.teams).await?;
    app.apply_projects_without_members(&config.projects, &team_ids)
        .await?;
    app.apply_project_secrets(&config.projects).await?;
    app.apply_project_git_https_credentials(&config.projects)
        .await?;
    app.apply_roles(&config.roles).await?;
    app.apply_project_members(&config.projects).await?;
    // Create connections to Git hosts before tasks. Tasks and `open_pr` actions look up a connection by name when they apply state (#332).
    app.apply_git_host_connections(&config.git_host_connections, &team_ids)
        .await?;
    app.apply_tasks(&config.tasks).await?;
    app.apply_caches(&config.caches, &team_ids).await?;
    app.apply_project_teams(&config.projects, &team_ids).await?;
    app.apply_cache_teams(&config.caches, &team_ids).await?;
    app.apply_api_keys(&config.api_keys).await?;
    app.apply_workers(&config.workers, &team_ids).await?;
    app.unmark_removed_entities(config, delete_state).await?;
    transaction.commit().await?;

    tracing::info!("State applied successfully");
    Ok(())
}

struct StateApplicator<'a> {
    db: &'a DatabaseTransaction,
    crypt_secret_file: &'a str,
    credentials_dir: &'a Path,
    email_enabled: bool,
    user_url_destination_allowlist: &'a gradient_util::http_validation::UserUrlDestinationAllowlist,
}
