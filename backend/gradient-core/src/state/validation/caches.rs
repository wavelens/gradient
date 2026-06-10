/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_db::permissions::CachePermission;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for cache in lookup.config.caches.values() {
        if !lookup.user_exists(&cache.created_by) {
            errors.push(
                format!("caches.{}.created_by", cache.name),
                format!("User '{}' does not exist", cache.created_by),
            );
        }

        for org_name in &cache.organizations {
            if !lookup.org_exists(org_name) {
                errors.push(
                    format!("caches.{}.organizations", cache.name),
                    format!("Organization '{}' does not exist", org_name),
                );
            }
        }

        let mut role_names_seen: HashSet<&str> = HashSet::new();
        for role in &cache.roles {
            if matches!(role.name.as_str(), "Admin" | "Write" | "View") {
                errors.push(
                    format!("caches.{}.roles.{}.name", cache.name, role.name),
                    format!(
                        "Role name '{}' collides with a built-in cache role; pick a different name.",
                        role.name
                    ),
                );
            }
            if role.permissions.is_empty() {
                errors.push(
                    format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                    "At least one permission must be declared.",
                );
            }
            for wire in &role.permissions {
                if CachePermission::from_wire_name(wire).is_none() {
                    errors.push(
                        format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                        format!("Unknown cache permission '{}'", wire),
                    );
                }
            }
            if !role_names_seen.insert(role.name.as_str()) {
                errors.push(
                    format!("caches.{}.roles.{}.name", cache.name, role.name),
                    format!("Duplicate role name '{}' in cache '{}'", role.name, cache.name),
                );
            }
        }

        let declared_role_names: HashSet<&str> =
            cache.roles.iter().map(|r| r.name.as_str()).collect();
        for member in &cache.members {
            if !lookup.user_exists(&member.user) {
                errors.push(
                    format!("caches.{}.members.{}.user", cache.name, member.user),
                    format!("User '{}' does not exist", member.user),
                );
            }
            let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
            if !builtin && !declared_role_names.contains(member.role.as_str()) {
                errors.push(
                    format!("caches.{}.members.{}.role", cache.name, member.user),
                    format!("Role '{}' not found in cache '{}'", member.role, cache.name),
                );
            }
        }
    }
}
