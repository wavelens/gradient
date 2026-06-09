/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::ci::GITHUB_APP_INTEGRATION_NAME;
use crate::types::triggers::TriggerType;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    for project in config.projects.values() {
        if !lookup.org_exists(&project.organization) {
            errors.push(
                format!("projects.{}.organization", project.name),
                format!("Organization '{}' does not exist", project.organization),
            );
        }

        if !lookup.user_exists(&project.created_by) {
            errors.push(
                format!("projects.{}.created_by", project.name),
                format!("User '{}' does not exist", project.created_by),
            );
        }

        if !project.repository.starts_with("http") && !project.repository.starts_with("git") {
            errors.push(
                format!("projects.{}.repository", project.name),
                "Repository URL must start with http or git",
            );
        }

        if project.keep_evaluations < 1 {
            errors.push(
                format!("projects.{}.keep_evaluations", project.name),
                "keep_evaluations must be at least 1",
            );
        }

        let mut action_names: HashSet<&str> = HashSet::new();
        for action in &project.actions {
            if !matches!(
                action.action_type.as_str(),
                "send_mail" | "send_web_request" | "forge_status_report"
            ) {
                errors.push(
                    format!("projects.{}.actions.{}.type", project.name, action.name),
                    format!(
                        "Invalid action type '{}': expected send_mail/send_web_request/forge_status_report",
                        action.action_type
                    ),
                );
            }
            if action.action_type == "forge_status_report" && !action.events.is_empty() {
                errors.push(
                    format!("projects.{}.actions.{}.events", project.name, action.name),
                    "forge_status_report actions cannot carry custom events",
                );
            }
            if !action_names.insert(action.name.as_str()) {
                errors.push(
                    format!("projects.{}.actions.{}.name", project.name, action.name),
                    format!(
                        "Duplicate action name '{}' in project '{}'",
                        action.name, project.name
                    ),
                );
            }
        }

        // Reporter triggers resolve their `integration` against the org's
        // inbound integrations at apply time; catch a missing/outbound/typo
        // reference here so it fails validation instead of mid-apply (#332).
        for trigger in project.triggers.iter().flatten() {
            if !matches!(
                trigger.trigger_type,
                TriggerType::ReporterPush | TriggerType::ReporterPullRequest
            ) {
                continue;
            }
            let Some(name) = &trigger.integration else {
                errors.push(
                    format!("projects.{}.triggers", project.name),
                    "reporter_push/reporter_pull_request triggers require an `integration`",
                );
                continue;
            };
            if name == GITHUB_APP_INTEGRATION_NAME {
                continue;
            }
            let declared_inbound = config.integrations.values().any(|i| {
                i.name == *name && i.organization == project.organization && i.kind == "inbound"
            });
            if !declared_inbound {
                errors.push(
                    format!("projects.{}.triggers", project.name),
                    format!(
                        "Reporter trigger references integration '{}' which is not a declared inbound integration in organization '{}'",
                        name, project.organization
                    ),
                );
            }
        }
    }
}
