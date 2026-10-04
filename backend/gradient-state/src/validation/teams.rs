/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use std::collections::HashSet;

const PROJECT_ROLES: [&str; 3] = ["Admin", "Write", "View"];

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;

    for team in config.teams.values() {
        let mut seen = HashSet::new();
        for member in &team.members {
            let field = format!("teams.{}.members.{}", team.name, member.user);
            if !lookup.user_exists(&member.user) {
                errors.push(
                    format!("{field}.user"),
                    format!("User '{}' does not exist", member.user),
                );
            }
            if !matches!(member.role.as_str(), "Admin" | "Member") {
                errors.push(format!("{field}.role"), "Role must be Admin or Member");
            }
            if !seen.insert(member.user.as_str()) {
                errors.push(
                    format!("{field}.user"),
                    format!("Duplicate member entry for user '{}'", member.user),
                );
            }
        }

        let grant = &team.new_projects;
        if grant.users && grant.role.is_none() {
            errors.push(
                format!("teams.{}.new_projects.role", team.name),
                "A role is required when new projects grant users",
            );
        }
        if let Some(role) = &grant.role
            && !PROJECT_ROLES.contains(&role.as_str())
        {
            errors.push(
                format!("teams.{}.new_projects.role", team.name),
                "Role must be Admin, Write or View",
            );
        }
    }

    for project in config.projects.values() {
        let declared: HashSet<&str> = config
            .roles
            .values()
            .filter(|r| r.project == project.name)
            .map(|r| r.name.as_str())
            .collect();
        for grant in &project.teams {
            let field = format!("projects.{}.teams.{}", project.name, grant.team);
            if !lookup.team_exists(&grant.team) {
                errors.push(
                    format!("{field}.team"),
                    format!("Team '{}' does not exist", grant.team),
                );
            }
            if !grant.users && !grant.workers {
                errors.push(field.clone(), "A grant includes users, workers or both");
            }
            match (&grant.role, grant.users) {
                (None, true) => errors.push(
                    format!("{field}.role"),
                    "A role is required when the grant includes users",
                ),
                (Some(role), _)
                    if !PROJECT_ROLES.contains(&role.as_str())
                        && !declared.contains(role.as_str()) =>
                {
                    errors.push(
                        format!("{field}.role"),
                        format!("Role '{}' not found for project '{}'", role, project.name),
                    );
                }
                _ => {}
            }
        }
    }

    for cache in config.caches.values() {
        let declared: HashSet<&str> = cache.roles.iter().map(|r| r.name.as_str()).collect();
        for grant in &cache.teams {
            let field = format!("caches.{}.teams.{}", cache.name, grant.team);
            if !lookup.team_exists(&grant.team) {
                errors.push(
                    format!("{field}.team"),
                    format!("Team '{}' does not exist", grant.team),
                );
            }
            if !PROJECT_ROLES.contains(&grant.role.as_str())
                && !declared.contains(grant.role.as_str())
            {
                errors.push(
                    format!("{field}.role"),
                    format!("Role '{}' not found in cache '{}'", grant.role, cache.name),
                );
            }
        }
    }
}
