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
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, QueryFilter, Set,
};
use std::collections::HashMap;

pub(crate) use credentials::{
    derive_public_key, parse_api_key_hash, parse_password_phc, read_credential,
};
pub(crate) use lookups::{inbound_integrations_by_name, lookup_id, outbound_integrations_by_name};

pub(crate) type DynError = Box<dyn std::error::Error>;

#[derive(Debug, Clone)]
pub struct PendingProjectMembership {
    pub project: ProjectId,
    pub role: RoleId,
}

pub type PendingProjectMemberships = HashMap<String, Vec<PendingProjectMembership>>;

pub struct StateApplyResult {
    pub pending: PendingProjectMemberships,
}

pub(super) async fn apply_state_to_database(
    db: &DatabaseConnection,
    config: &StateConfiguration,
    crypt_secret_file: &str,
    delete_state: bool,
    email_enabled: bool,
) -> Result<StateApplyResult, DynError> {
    tracing::info!("Applying state to database");

    let app = StateApplicator {
        db,
        crypt_secret_file,
        email_enabled,
    };

    let mut pending: PendingProjectMemberships = HashMap::new();

    app.apply_users(&config.users).await?;
    // Teams must exist before projects, for their new-project grants (the local worker's `server`).
    let team_ids = app.apply_teams(&config.teams).await?;
    app.apply_projects_without_members(&config.projects).await?;
    app.apply_roles(&config.roles).await?;
    app.apply_project_members(&config.projects, &mut pending)
        .await?;
    // Integrations must land before tasks. Task triggers and `git_host_status_report` actions are
    // resolving integrations by name at apply time (#332).
    app.apply_integrations(&config.integrations).await?;
    app.apply_tasks(&config.tasks).await?;
    app.apply_caches(&config.caches).await?;
    app.apply_project_teams(&config.projects, &team_ids).await?;
    app.apply_cache_teams(&config.caches, &team_ids).await?;
    app.apply_api_keys(&config.api_keys).await?;
    app.apply_workers(&config.workers, &team_ids).await?;
    app.unmark_removed_entities(config, delete_state).await?;

    tracing::info!("State applied successfully");
    Ok(StateApplyResult { pending })
}

struct StateApplicator<'a> {
    db: &'a DatabaseConnection,
    crypt_secret_file: &'a str,
    email_enabled: bool,
}

pub async fn apply_pending_project_memberships<C: ConnectionTrait>(
    db: &C,
    pending: &PendingProjectMemberships,
    username: &str,
    user_id: UserId,
) -> Result<usize, sea_orm::DbErr> {
    let Some(entries) = pending.get(username) else {
        return Ok(0);
    };
    let mut applied = 0usize;
    for entry in entries {
        let existing = project_user::Entity::find()
            .filter(project_user::Column::Project.eq(entry.project))
            .filter(project_user::Column::User.eq(user_id))
            .one(db)
            .await?;
        match existing {
            Some(row) if row.role == entry.role => {}
            Some(row) => {
                let mut active: project_user::ActiveModel = row.into();
                active.role = Set(entry.role);
                active.update(db).await?;
                applied += 1;
            }
            None => {
                project_user::Model {
                    id: ProjectUserId::now_v7(),
                    project: entry.project,
                    user: user_id,
                    role: entry.role,
                }
                .into_active_model()
                .insert(db)
                .await?;
                applied += 1;
            }
        }
    }
    if applied > 0 {
        tracing::info!(
            username,
            count = applied,
            "Applied pending state-managed project memberships for newly-registered user"
        );
    }
    Ok(applied)
}
