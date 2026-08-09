/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_db::permissions::Permission;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let mut role_keys_seen_per_project: HashSet<(String, String)> = HashSet::new();
    for role in lookup.config.roles.values() {
        if !lookup.project_exists(&role.project) {
            errors.push(
                format!("roles.{}.project", role.name),
                format!("Project '{}' does not exist", role.project),
            );
        }
        if role.permissions.is_empty() {
            errors.push(
                format!("roles.{}.permissions", role.name),
                "At least one permission must be declared.",
            );
        }
        for wire in &role.permissions {
            if Permission::from_wire_name(wire).is_none() {
                errors.push(
                    format!("roles.{}.permissions", role.name),
                    format!("Unknown permission '{}'", wire),
                );
            }
        }
        if matches!(role.name.as_str(), "Admin" | "Write" | "View") {
            errors.push(
                format!("roles.{}.name", role.name),
                format!(
                    "Role name '{}' collides with a built-in role; pick a different name.",
                    role.name
                ),
            );
        }
        let key = (role.project.clone(), role.name.clone());
        if !role_keys_seen_per_project.insert(key) {
            errors.push(
                format!("roles.{}.name", role.name),
                format!(
                    "Duplicate role '{}' in project '{}'",
                    role.name, role.project
                ),
            );
        }
    }
}
