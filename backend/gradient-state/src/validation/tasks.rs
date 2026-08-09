/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_types::triggers::TriggerType;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    for task in config.tasks.values() {
        if !lookup.org_exists(&task.organization) {
            errors.push(
                format!("tasks.{}.organization", task.name),
                format!("Organization '{}' does not exist", task.organization),
            );
        }

        if !lookup.user_exists(&task.created_by) {
            errors.push(
                format!("tasks.{}.created_by", task.name),
                format!("User '{}' does not exist", task.created_by),
            );
        }

        if !task.repository.starts_with("http") && !task.repository.starts_with("git") {
            errors.push(
                format!("tasks.{}.repository", task.name),
                "Repository URL must start with http or git",
            );
        }

        if task.keep_evaluations < 1 {
            errors.push(
                format!("tasks.{}.keep_evaluations", task.name),
                "keep_evaluations must be at least 1",
            );
        }

        let mut action_names: HashSet<&str> = HashSet::new();
        for action in &task.actions {
            if !matches!(
                action.action_type.as_str(),
                "send_mail" | "send_web_request" | "forge_status_report" | "open_pr"
            ) {
                errors.push(
                    format!("tasks.{}.actions.{}.type", task.name, action.name),
                    format!(
                        "Invalid action type '{}': expected send_mail/send_web_request/forge_status_report/open_pr",
                        action.action_type
                    ),
                );
            }
            if matches!(
                action.action_type.as_str(),
                "forge_status_report" | "open_pr"
            ) && !action.events.is_empty()
            {
                errors.push(
                    format!("tasks.{}.actions.{}.events", task.name, action.name),
                    format!("{} actions cannot carry custom events", action.action_type),
                );
            }
            if !action_names.insert(action.name.as_str()) {
                errors.push(
                    format!("tasks.{}.actions.{}.name", task.name, action.name),
                    format!(
                        "Duplicate action name '{}' in task '{}'",
                        action.name, task.name
                    ),
                );
            }
        }

        // Reporter triggers resolve their `integration` against the org's
        // inbound integrations at apply time; catch a missing/outbound/typo
        // reference here so it fails validation instead of mid-apply (#332).
        for trigger in task.triggers.iter().flatten() {
            if !matches!(
                trigger.trigger_type,
                TriggerType::ReporterPush | TriggerType::ReporterPullRequest
            ) {
                continue;
            }
            let Some(name) = &trigger.integration else {
                errors.push(
                    format!("tasks.{}.triggers", task.name),
                    "reporter_push/reporter_pull_request triggers require an `integration`",
                );
                continue;
            };
            if name == "github" || name.starts_with("github-") {
                continue;
            }
            let declared_inbound = config.integrations.values().any(|i| {
                i.name == *name && i.organization == task.organization && i.kind == "inbound"
            });
            if !declared_inbound {
                errors.push(
                    format!("tasks.{}.triggers", task.name),
                    format!(
                        "Reporter trigger references integration '{}' which is not a declared inbound integration in organization '{}'",
                        name, task.organization
                    ),
                );
            }
        }
    }
}
