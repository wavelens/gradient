/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::lookup_id;
use crate::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    // ── apply_roles ───────────────────────────────────────────────────────────

    pub(crate) async fn apply_roles(
        &self,
        state_roles: &HashMap<String, StateRole>,
    ) -> Result<HashMap<(String, String), (OrganizationId, RoleId)>, DynError> {
        let org_lookup = self.org_lookup().await?;
        let mut role_ids: HashMap<(String, String), (OrganizationId, RoleId)> = HashMap::new();

        for state_role in state_roles.values() {
            let org_id = lookup_id(&org_lookup, &state_role.organization, "Organization")?;

            let mut perms = Vec::with_capacity(state_role.permissions.len());
            for wire in &state_role.permissions {
                let p = gradient_db::permissions::Permission::from_wire_name(wire).ok_or_else(|| {
                    format!(
                        "Role '{}' references unknown permission '{}'",
                        state_role.name, wire
                    )
                })?;
                perms.push(p);
            }
            if perms.is_empty() {
                return Err(format!(
                    "Role '{}' must declare at least one permission",
                    state_role.name
                )
                .into());
            }
            let mask = gradient_db::permissions::mask_from(&perms);

            let existing = role::Entity::find()
                .filter(role::Column::Name.eq(&state_role.name))
                .filter(role::Column::Organization.eq(org_id))
                .one(self.db)
                .await?;

            let role_id = if let Some(existing) = existing {
                let id = existing.id;
                let mut active: role::ActiveModel = existing.into();
                active.permission = Set(mask);
                active.managed = Set(true);
                active.update(self.db).await?;
                tracing::info!(name = %state_role.name, "Updated managed role");
                id
            } else {
                let id = RoleId::now_v7();
                let active = role::Model {
                    id,
                    name: state_role.name.clone(),
                    organization: Some(org_id),
                    permission: mask,
                    managed: true,
                }
                .into_active_model();

                active.insert(self.db).await?;
                tracing::info!(name = %state_role.name, "Created managed role");
                id
            };

            role_ids.insert(
                (state_role.organization.clone(), state_role.name.clone()),
                (org_id, role_id),
            );
        }

        Ok(role_ids)
    }
}
