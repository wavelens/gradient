/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    let mut project_ids_seen: HashSet<String> = HashSet::new();
    for project in config.projects.values() {
        if !lookup.user_exists(&project.created_by) {
            errors.push(
                format!("projects.{}.created_by", project.name),
                format!("User '{}' does not exist", project.created_by),
            );
        }

        if let Some(id) = &project.id {
            match id.trim().parse::<uuid::Uuid>() {
                Ok(parsed) => {
                    if !project_ids_seen.insert(parsed.to_string()) {
                        errors.push(
                            format!("projects.{}.id", project.name),
                            format!("Duplicate project id '{}'", id),
                        );
                    }
                }
                Err(_) => errors.push(
                    format!("projects.{}.id", project.name),
                    format!("Invalid UUID '{}'", id),
                ),
            }
        }

        let declared_project_role_names: HashSet<&str> = config
            .roles
            .values()
            .filter(|r| r.project == project.name)
            .map(|r| r.name.as_str())
            .collect();
        let mut member_users_seen: HashSet<&str> = HashSet::new();
        for member in &project.members {
            let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
            if !builtin && !declared_project_role_names.contains(member.role.as_str()) {
                errors.push(
                    format!("projects.{}.members.{}.role", project.name, member.user),
                    format!(
                        "Role '{}' not found for project '{}' (must be Admin/Write/View or a state-managed project role)",
                        member.role, project.name
                    ),
                );
            }
            if !member_users_seen.insert(member.user.as_str()) {
                errors.push(
                    format!("projects.{}.members.{}.user", project.name, member.user),
                    format!(
                        "Duplicate member entry for user '{}' in project '{}'",
                        member.user, project.name
                    ),
                );
            }
            // Note: missing user is intentionally not an error (issue #94).
        }
    }
}
