/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    let mut org_ids_seen: HashSet<String> = HashSet::new();
    for org in config.organizations.values() {
        if !lookup.user_exists(&org.created_by) {
            errors.push(
                format!("organizations.{}.created_by", org.name),
                format!("User '{}' does not exist", org.created_by),
            );
        }

        if let Some(id) = &org.id {
            match id.trim().parse::<uuid::Uuid>() {
                Ok(parsed) => {
                    if !org_ids_seen.insert(parsed.to_string()) {
                        errors.push(
                            format!("organizations.{}.id", org.name),
                            format!("Duplicate organization id '{}'", id),
                        );
                    }
                }
                Err(_) => errors.push(
                    format!("organizations.{}.id", org.name),
                    format!("Invalid UUID '{}'", id),
                ),
            }
        }

        let declared_org_role_names: HashSet<&str> = config
            .roles
            .values()
            .filter(|r| r.organization == org.name)
            .map(|r| r.name.as_str())
            .collect();
        let mut member_users_seen: HashSet<&str> = HashSet::new();
        for member in &org.members {
            let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
            if !builtin && !declared_org_role_names.contains(member.role.as_str()) {
                errors.push(
                    format!("organizations.{}.members.{}.role", org.name, member.user),
                    format!(
                        "Role '{}' not found for organization '{}' (must be Admin/Write/View or a state-managed org role)",
                        member.role, org.name
                    ),
                );
            }
            if !member_users_seen.insert(member.user.as_str()) {
                errors.push(
                    format!("organizations.{}.members.{}.user", org.name, member.user),
                    format!(
                        "Duplicate member entry for user '{}' in organization '{}'",
                        member.user, org.name
                    ),
                );
            }
            // Note: missing user is intentionally not an error (issue #94).
        }
    }
}
