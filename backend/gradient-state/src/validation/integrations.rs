/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};

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
        if !matches!(integration.kind.as_str(), "inbound" | "outbound") {
            errors.push(
                format!("integrations.{}.kind", integration.name),
                format!(
                    "Invalid kind '{}': expected 'inbound' or 'outbound'",
                    integration.kind
                ),
            );
        }
        if !matches!(
            integration.forge_type.as_str(),
            "gitea" | "forgejo" | "gitlab" | "github"
        ) {
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
