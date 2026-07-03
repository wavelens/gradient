/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_ci::integration_lookup::IntegrationKind;
use gradient_types::forge::ForgeType;

fn parse_integration_kind(s: &str) -> Option<IntegrationKind> {
    match s {
        "inbound" => Some(IntegrationKind::Inbound),
        "outbound" => Some(IntegrationKind::Outbound),
        _ => None,
    }
}

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for integration in lookup.config.integrations.values() {
        if !lookup.org_exists(&integration.organization) {
            errors.push(
                format!("integrations.{}.organization", integration.name),
                format!("Organization '{}' does not exist", integration.organization),
            );
        }
        if !lookup.user_exists(&integration.created_by) {
            errors.push(
                format!("integrations.{}.created_by", integration.name),
                format!("User '{}' does not exist", integration.created_by),
            );
        }
        if parse_integration_kind(&integration.kind).is_none() {
            errors.push(
                format!("integrations.{}.kind", integration.name),
                format!(
                    "Invalid kind '{}': expected 'inbound' or 'outbound'",
                    integration.kind
                ),
            );
        }
        if ForgeType::from_path_segment(&integration.forge_type).is_none() {
            errors.push(
                format!("integrations.{}.forge_type", integration.name),
                format!(
                    "Invalid forge_type '{}': expected gitea/forgejo/gitlab/github",
                    integration.forge_type
                ),
            );
        }

        if integration.forge_type == "github"
            && integration.installation_id.is_none_or(|id| id <= 0)
        {
            errors.push(
                format!("integrations.{}.installation_id", integration.name),
                "forge_type 'github' requires a positive installation_id",
            );
        }
    }
}
