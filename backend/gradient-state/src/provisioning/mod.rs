/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Applies a validated [`StateConfiguration`] to the database. The entry point
//! [`apply_state_to_database`] drives the per-entity appliers ([`entities`]) via
//! the shared [`StateApplicator`], then reconciles drift ([`reconciliation`]).
//! Credential reading lives in [`credentials`], name lookups in [`lookups`].

mod credentials;
mod entities;
mod lookups;
mod reconciliation;

use crate::config::StateConfiguration;
use gradient_types::*;
use gradient_entity::*;
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

/// Org membership declared in state for a user who did not exist at apply
/// time. Drained per-username when the user is later registered or signs
/// in via OIDC for the first time.
#[derive(Debug, Clone)]
pub struct PendingOrgMembership {
    pub organization: OrganizationId,
    pub role: RoleId,
}

pub type PendingOrgMemberships = HashMap<String, Vec<PendingOrgMembership>>;

/// Outcome of applying declarative state: memberships deferred until their user
/// exists, and the OIDC group → role grants resolved from `StateRole.oidc_group`.
pub struct StateApplyResult {
    pub pending: PendingOrgMemberships,
    pub oidc_group_roles: crate::OidcGroupRoles,
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

    let mut pending: PendingOrgMemberships = HashMap::new();

    app.apply_users(&config.users).await?;
    app.apply_organizations_without_members(&config.organizations)
        .await?;
    let role_ids = app.apply_roles(&config.roles).await?;
    app.apply_organization_members(&config.organizations, &mut pending)
        .await?;
    // Integrations must land before projects: project triggers
    // (reporter_push/reporter_pull_request) and `forge_status_report` actions
    // resolve integrations by name from the DB at apply time (#332).
    app.apply_integrations(&config.integrations).await?;
    app.apply_projects(&config.projects).await?;
    app.apply_caches(&config.caches).await?;
    app.apply_api_keys(&config.api_keys).await?;
    app.apply_workers(&config.workers).await?;
    app.unmark_removed_entities(config, delete_state).await?;

    let oidc_group_roles = super::resolve_oidc_group_roles(config, &role_ids);

    tracing::info!("State applied successfully");
    Ok(StateApplyResult {
        pending,
        oidc_group_roles,
    })
}

/// Applies a [`StateConfiguration`] to the database.
///
/// Captures the database connection and crypt secret so each `apply_*` method
/// does not repeat those parameters.
struct StateApplicator<'a> {
    db: &'a DatabaseConnection,
    crypt_secret_file: &'a str,
    email_enabled: bool,
}

/// Apply any pending state-managed org memberships for `username` against
/// `user_id`. Idempotent: existing rows are updated to the declared role,
/// missing rows are inserted. Returns the number of memberships applied
/// (`Ok(0)` when the username has no pending entries).
///
/// Called from the user-creation paths (`POST /user` and OIDC first-login)
/// so a member declared in state for a not-yet-registered user becomes
/// effective the instant that user joins.
pub async fn apply_pending_org_memberships<C: ConnectionTrait>(
    db: &C,
    pending: &PendingOrgMemberships,
    username: &str,
    user_id: UserId,
) -> Result<usize, sea_orm::DbErr> {
    let Some(entries) = pending.get(username) else {
        return Ok(0);
    };
    let mut applied = 0usize;
    for entry in entries {
        let existing = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(entry.organization))
            .filter(organization_user::Column::User.eq(user_id))
            .one(db)
            .await?;
        match existing {
            Some(row) if row.role == entry.role => {}
            Some(row) => {
                let mut active: organization_user::ActiveModel = row.into();
                active.role = Set(entry.role);
                active.update(db).await?;
                applied += 1;
            }
            None => {
                organization_user::Model {
                    id: OrganizationUserId::now_v7(),
                    organization: entry.organization,
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
            "Applied pending state-managed org memberships for newly-registered user"
        );
    }
    Ok(applied)
}

#[cfg(test)]
mod pending_membership_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn apply_pending_returns_zero_for_unknown_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let pending: PendingOrgMemberships = HashMap::new();
        let count = apply_pending_org_memberships(&db, &pending, "ghost", UserId::now_v7())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
