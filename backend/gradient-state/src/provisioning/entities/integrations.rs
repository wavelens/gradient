/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{lookup_id, read_credential};
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_ci::IntegrationKind;
use gradient_types::ForgeType;
use crate::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    // ── apply_integrations ────────────────────────────────────────────────────

    pub(crate) async fn apply_integrations(
        &self,
        state_integrations: &HashMap<String, StateIntegration>,
    ) -> Result<(), DynError> {
        if state_integrations.is_empty() {
            return Ok(());
        }

        let org_map = self.org_lookup().await?;
        let user_map = self.user_lookup().await?;

        for state_int in state_integrations.values() {
            let org_id = org_map
                .get(&state_int.organization)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "Integration '{}' references unknown organization '{}'",
                        state_int.name, state_int.organization
                    )
                })?;

            let created_by_id = lookup_id(&user_map, &state_int.created_by, "User")?;

            let kind = match state_int.kind.as_str() {
                "inbound" => IntegrationKind::Inbound,
                "outbound" => IntegrationKind::Outbound,
                other => {
                    return Err(format!(
                        "Integration '{}' has invalid kind '{}': expected 'inbound' or 'outbound'",
                        state_int.name, other
                    )
                    .into());
                }
            };

            let forge = ForgeType::from_path_segment(&state_int.forge_type).ok_or_else(|| {
                format!(
                    "Integration '{}' has invalid forge_type '{}': expected gitea/forgejo/gitlab",
                    state_int.name, state_int.forge_type
                )
            })?;
            if matches!(forge, ForgeType::GitHub) {
                return Err(format!(
                    "Integration '{}' has forge_type 'github': GitHub integrations are managed \
                     automatically via `github_installations` on the org.",
                    state_int.name
                )
                .into());
            }
            let encrypted_secret = self.read_and_encrypt_integration_field(
                state_int.secret_file.as_deref(),
                &state_int.name,
                "secret",
            )?;
            let encrypted_token = self.read_and_encrypt_integration_field(
                state_int.access_token_file.as_deref(),
                &state_int.name,
                "token",
            )?;

            let endpoint = state_int
                .endpoint_url
                .as_deref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let existing = integration::Entity::find()
                .filter(integration::Column::Organization.eq(org_id))
                .filter(integration::Column::Kind.eq(i16::from(kind)))
                .filter(integration::Column::Name.eq(&state_int.name))
                .one(self.db)
                .await?;

            let display_name = state_int
                .display_name
                .clone()
                .unwrap_or_else(|| state_int.name.clone());

            if let Some(existing) = existing {
                let mut active: integration::ActiveModel = existing.into();
                active.display_name = Set(display_name);
                active.forge_type = Set(i16::from(forge));
                active.endpoint_url = Set(endpoint);
                active.secret = Set(encrypted_secret);
                active.access_token = Set(encrypted_token);
                active.created_by = Set(created_by_id);
                active.update(self.db).await?;
                tracing::info!(name = %state_int.name, "Updated managed integration");
            } else {
                let row = integration::Model {
                    id: IntegrationId::now_v7(),
                    organization: org_id,
                    name: state_int.name.clone(),
                    display_name,
                    kind: i16::from(kind),
                    forge_type: i16::from(forge),
                    secret: encrypted_secret,
                    endpoint_url: endpoint,
                    access_token: encrypted_token,
                    created_by: created_by_id,
                    created_at: now(),
                    ..Default::default()
                }
                .into_active_model();

                row.insert(self.db).await?;
                tracing::info!(name = %state_int.name, "Created managed integration");
            }
        }

        Ok(())
    }

    /// Read `${creds}/gradient_integration_${name}_${suffix}` and encrypt its
    /// trimmed contents with the webhook secret. Returns `Ok(None)` when the
    /// state config did not declare a credential file (`field_set` is `None`).
    pub(crate) fn read_and_encrypt_integration_field(
        &self,
        field_set: Option<&str>,
        int_name: &str,
        suffix: &str,
    ) -> Result<Option<String>, DynError> {
        if field_set.is_none() {
            return Ok(None);
        }
        let label = format!("integration {} file", suffix);
        let (plain, _) = read_credential("integration", int_name, suffix, &label)?;
        let encrypted =
            encrypt_secret_with_file(self.crypt_secret_file, plain.trim()).map_err(|e| {
                format!(
                    "Failed to encrypt {} for integration '{}': {}",
                    suffix, int_name, e
                )
            })?;
        Ok(Some(encrypted))
    }
}
