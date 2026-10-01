/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_ci::integration_lookup::IntegrationKind;
use gradient_types::git_host::GitHostType;

fn parse_integration_kind(s: &str) -> Option<IntegrationKind> {
    match s {
        "inbound" => Some(IntegrationKind::Inbound),
        "outbound" => Some(IntegrationKind::Outbound),
        _ => None,
    }
}

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for integration in lookup.config.integrations.values() {
        if !lookup.project_exists(&integration.project) {
            errors.push(
                format!("integrations.{}.project", integration.name),
                format!("Project '{}' does not exist", integration.project),
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
        if GitHostType::from_path_segment(&integration.git_host_type).is_none() {
            errors.push(
                format!("integrations.{}.git_host_type", integration.name),
                format!(
                    "Invalid git_host_type '{}': expected gitea/forgejo/gitlab/github",
                    integration.git_host_type
                ),
            );
        }

        if integration.git_host_type == "github"
            && integration.installation_id.is_none_or(|id| id <= 0)
        {
            errors.push(
                format!("integrations.{}.installation_id", integration.name),
                "git_host_type 'github' requires a positive installation_id",
            );
        }
    }
}
