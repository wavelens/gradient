/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateApplicator;
use super::super::{DynError, PendingOrgMembership, PendingOrgMemberships};
use super::super::{derive_public_key, lookup_id, read_credential};
use crate::config::*;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    // ── apply_organizations_without_members ───────────────────────────────────

    /// Create/update the `organization` row (and seed the GitHub App
    /// integration if needed). Membership reconciliation happens later in
    /// `apply_organization_members`, after `apply_roles` so custom org roles
    /// referenced by `members` can be resolved against rows inserted in the
    /// same apply pass.
    pub(crate) async fn apply_organizations_without_members(
        &self,
        state_orgs: &HashMap<String, StateOrganization>,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        for state_org in state_orgs.values() {
            let (private_key, _) =
                read_credential("org", &state_org.name, "private_key", "private key file")?;
            let private_key = private_key.trim();

            let public_key = derive_public_key(private_key)?;
            let encrypted_private_key = self.encrypt_to_b64(private_key, "SSH private key")?;

            let created_by_id = lookup_id(&user_map, &state_org.created_by, "User")?;

            let existing_org = organization::Entity::find()
                .filter(organization::Column::Name.eq(&state_org.name))
                .one(self.db)
                .await?;

            let now = now();

            let declared_id = match &state_org.id {
                Some(s) => Some(s.trim().parse::<OrganizationId>().map_err(|e| {
                    format!(
                        "Organization '{}' has an invalid id '{}': {}",
                        state_org.name, s, e
                    )
                })?),
                None => None,
            };

            let org_id = if let Some(existing) = existing_org {
                let org_id = existing.id;
                if let Some(declared) = declared_id
                    && declared != org_id
                {
                    return Err(format!(
                        "Organization '{}' already exists with id {} but state declares id {}; the id is immutable",
                        state_org.name, org_id, declared
                    )
                    .into());
                }
                let mut org: organization::ActiveModel = existing.into();
                org.display_name = Set(state_org.display_name.clone());
                org.description = Set(state_org.description.clone().unwrap_or_default());
                org.public_key = Set(public_key);
                org.private_key = Set(encrypted_private_key.clone());
                org.created_by = Set(created_by_id);
                org.public = Set(state_org.public);
                org.hide_build_requests = Set(state_org.hide_build_requests);
                // Only overwrite github_installation_id when state declares
                // it; otherwise leave the existing value (likely set by the
                // install webhook) intact.
                if let Some(id) = state_org.github_installation_id {
                    org.github_installation_id = Set(Some(id));
                }
                org.managed = Set(true);
                org.update(self.db).await?;
                tracing::info!(name = %state_org.name, "Updated managed organization");
                org_id
            } else {
                let org_id = declared_id.unwrap_or_else(OrganizationId::now_v7);
                let org = organization::ActiveModel {
                    id: Set(org_id),
                    name: Set(state_org.name.clone()),
                    display_name: Set(state_org.display_name.clone()),
                    description: Set(state_org.description.clone().unwrap_or_default()),
                    public_key: Set(public_key),
                    private_key: Set(encrypted_private_key),
                    public: Set(state_org.public),
                    hide_build_requests: Set(state_org.hide_build_requests),
                    created_by: Set(created_by_id),
                    created_at: Set(now),
                    managed: Set(true),
                    github_installation_id: Set(state_org.github_installation_id),
                };
                org.insert(self.db).await?;
                tracing::info!(name = %state_org.name, "Created managed organization");
                org_id
            };

            // Seed the auto-managed GitHub App integration rows whenever this
            // org has (or just acquired) an installation id. Idempotent.
            let installation_known = match organization::Entity::find_by_id(org_id)
                .one(self.db)
                .await?
            {
                Some(o) => o.github_installation_id.is_some(),
                None => false,
            };
            if installation_known {
                gradient_ci::ensure_github_app_integrations(self.db, org_id, created_by_id).await?;
            }
        }

        Ok(())
    }

    // ── apply_organization_members ────────────────────────────────────────────

    /// Reconcile `organization_user` rows for every state-managed org.
    ///
    /// When `state_org.members` is empty, the legacy behavior applies:
    /// `created_by` is added as Admin if no row exists. When `members` is
    /// non-empty, the declared list is authoritative — see
    /// [`StateApplicator::apply_org_members`] for the per-org logic.
    pub(crate) async fn apply_organization_members(
        &self,
        state_orgs: &HashMap<String, StateOrganization>,
        pending: &mut PendingOrgMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;
        let org_map = self.org_lookup().await?;

        for state_org in state_orgs.values() {
            let org_id = lookup_id(&org_map, &state_org.name, "Organization")?;
            let created_by_id = lookup_id(&user_map, &state_org.created_by, "User")?;

            if state_org.members.is_empty() {
                let existing = organization_user::Entity::find()
                    .filter(organization_user::Column::Organization.eq(org_id))
                    .filter(organization_user::Column::User.eq(created_by_id))
                    .one(self.db)
                    .await?;

                if existing.is_none() {
                    organization_user::ActiveModel {
                        id: Set(OrganizationUserId::now_v7()),
                        organization: Set(org_id),
                        user: Set(created_by_id),
                        role: Set(BASE_ROLE_ADMIN_ID),
                    }
                    .insert(self.db)
                    .await?;
                    tracing::info!(
                        username = %state_org.created_by,
                        organization = %state_org.name,
                        "Added admin member to organization"
                    );
                }
            } else {
                self.apply_org_members(org_id, &state_org.name, &state_org.members, pending)
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to apply members for organization '{}': {}",
                            state_org.name, e
                        )
                    })?;
            }
        }

        Ok(())
    }

    /// Reconcile membership for a single state-managed organization whose
    /// `members` list is non-empty.
    ///
    /// - Missing users are recorded into `pending` and skipped (issue #94);
    ///   they'll be applied when the user later registers or signs in via
    ///   OIDC.
    /// - Built-in roles (`Admin`/`Write`/`View`) map to constant role IDs;
    ///   custom org roles resolve against `role` rows scoped to this org.
    /// - Drift: existing memberships not in the declared user set are
    ///   deleted. State owns the membership list when explicitly declared.
    pub(crate) async fn apply_org_members(
        &self,
        org_id: OrganizationId,
        org_name: &str,
        members: &[StateOrgMemberEntry],
        pending: &mut PendingOrgMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        let custom_roles: HashMap<String, RoleId> = role::Entity::find()
            .filter(role::Column::Organization.eq(org_id))
            .filter(role::Column::Managed.eq(true))
            .all(self.db)
            .await?
            .into_iter()
            .map(|r| (r.name, r.id))
            .collect();

        let mut declared_user_ids: HashSet<UserId> = HashSet::new();

        for member in members {
            let role_id = match member.role.as_str() {
                "Admin" => BASE_ROLE_ADMIN_ID,
                "Write" => BASE_ROLE_WRITE_ID,
                "View" => BASE_ROLE_VIEW_ID,
                name => *custom_roles.get(name).ok_or_else(|| -> DynError {
                    format!(
                        "Organization '{}' member '{}' references unknown role '{}'",
                        org_name, member.user, name
                    )
                    .into()
                })?,
            };

            match user_map.get(&member.user).copied() {
                Some(user_id) => {
                    declared_user_ids.insert(user_id);
                    let existing = organization_user::Entity::find()
                        .filter(organization_user::Column::Organization.eq(org_id))
                        .filter(organization_user::Column::User.eq(user_id))
                        .one(self.db)
                        .await?;
                    if let Some(row) = existing {
                        if row.role != role_id {
                            let mut active: organization_user::ActiveModel = row.into();
                            active.role = Set(role_id);
                            active.update(self.db).await?;
                            tracing::info!(
                                organization = %org_name,
                                user = %member.user,
                                "Updated organization membership role"
                            );
                        }
                    } else {
                        organization_user::ActiveModel {
                            id: Set(OrganizationUserId::now_v7()),
                            organization: Set(org_id),
                            user: Set(user_id),
                            role: Set(role_id),
                        }
                        .insert(self.db)
                        .await?;
                        tracing::info!(
                            organization = %org_name,
                            user = %member.user,
                            "Added organization member"
                        );
                    }
                }
                None => {
                    tracing::info!(
                        organization = %org_name,
                        user = %member.user,
                        "Declared member not yet registered; deferring until user creation"
                    );
                    pending
                        .entry(member.user.clone())
                        .or_default()
                        .push(PendingOrgMembership {
                            organization: org_id,
                            role: role_id,
                        });
                }
            }
        }

        let existing = organization_user::Entity::find()
            .filter(organization_user::Column::Organization.eq(org_id))
            .all(self.db)
            .await?;
        for row in existing {
            if !declared_user_ids.contains(&row.user) {
                let user_id = row.user;
                organization_user::Entity::delete_by_id(row.id)
                    .exec(self.db)
                    .await?;
                tracing::info!(
                    organization = %org_name,
                    %user_id,
                    "Removed organization member no longer in state"
                );
            }
        }

        Ok(())
    }
}
