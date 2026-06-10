/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{parse_api_key_hash, read_credential};
use crate::state::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    // ── apply_api_keys ────────────────────────────────────────────────────────

    pub(crate) async fn apply_api_keys(
        &self,
        state_api_keys: &HashMap<String, StateApiKey>,
    ) -> Result<(), DynError> {
        let user_lookup = self.user_lookup().await?;
        let org_lookup = self.org_lookup().await?;
        let now = now();

        for state_api_key in state_api_keys.values() {
            let owned_by_id = user_lookup
                .get(&state_api_key.owned_by)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "User '{}' not found for API key '{}'",
                        state_api_key.owned_by, state_api_key.name
                    )
                })?;

            let mut perms = Vec::with_capacity(state_api_key.permissions.len());
            for wire in &state_api_key.permissions {
                let p = crate::permissions::Permission::from_wire_name(wire).ok_or_else(|| {
                    format!(
                        "API key '{}' references unknown permission '{}'",
                        state_api_key.name, wire
                    )
                })?;
                perms.push(p);
            }
            if perms.is_empty() {
                return Err(format!(
                    "API key '{}' must declare at least one permission",
                    state_api_key.name
                )
                .into());
            }
            let mask = crate::permissions::mask_from(&perms);

            let pinned_org = match &state_api_key.organization {
                None => None,
                Some(name) => Some(org_lookup.get(name).copied().ok_or_else(|| {
                    format!(
                        "Organization '{}' referenced by API key '{}' not found",
                        name, state_api_key.name
                    )
                })?),
            };

            let (key_value, key_path) =
                read_credential("api", &state_api_key.name, "key", "API key file")?;
            let key_hash = parse_api_key_hash(&key_value, &key_path)?;

            let existing_api_key = api::Entity::find()
                .filter(api::Column::Name.eq(&state_api_key.name))
                .filter(api::Column::OwnedBy.eq(owned_by_id))
                .one(self.db)
                .await?;

            if let Some(api_key_model) = existing_api_key {
                let mut api_key: api::ActiveModel = api_key_model.into();
                api_key.key = Set(key_hash);
                api_key.managed = Set(true);
                api_key.permission = Set(mask);
                api_key.organization = Set(pinned_org);
                api_key.update(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Updated managed API key");
            } else {
                let api_key_model = api::ActiveModel {
                    id: Set(ApiId::now_v7()),
                    owned_by: Set(owned_by_id),
                    name: Set(state_api_key.name.clone()),
                    key: Set(key_hash),
                    last_used_at: Set(now),
                    created_at: Set(now),
                    managed: Set(true),
                    expires_at: Set(None),
                    revoked_at: Set(None),
                    permission: Set(mask),
                    organization: Set(pinned_org),
                    cache: Set(None),
                    allowed_ips: Set(None),
                };
                api_key_model.insert(self.db).await?;
                tracing::info!(name = %state_api_key.name, "Created managed API key");
            }
        }

        Ok(())
    }
}
