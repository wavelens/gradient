/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::permissions::Permission;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let mut role_keys_seen_per_org: HashSet<(String, String)> = HashSet::new();
    for role in lookup.config.roles.values() {
        if !lookup.org_exists(&role.organization) {
            errors.push(
                format!("roles.{}.organization", role.name),
                format!("Organization '{}' does not exist", role.organization),
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
        let key = (role.organization.clone(), role.name.clone());
        if !role_keys_seen_per_org.insert(key) {
            errors.push(
                format!("roles.{}.name", role.name),
                format!(
                    "Duplicate role '{}' in organization '{}'",
                    role.name, role.organization
                ),
            );
        }
    }
}
