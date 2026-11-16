/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::config::StateProject;
use gradient_types::https_git_url_host::normalize_bare_https_git_host;
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
            // A missing user is intentionally not an error (issue #94).
        }

        validate_project_https_git_credentials(project, errors);
    }
}

fn validate_project_https_git_credentials(project: &StateProject, errors: &mut ErrorCollector) {
    let mut hosts_seen: HashSet<String> = HashSet::new();
    for (index, credential) in project.git_https_credentials.iter().enumerate() {
        let field = format!("projects.{}.git_https_credentials.{index}", project.name);
        let host = &credential.host;
        match normalize_bare_https_git_host(host).map(|normalized| hosts_seen.insert(normalized)) {
            Some(false) => errors.push(
                format!("{field}.host"),
                format!("Duplicate credentials for host '{host}'"),
            ),
            Some(true) => {}
            None => errors.push(
                format!("{field}.host"),
                invalid_git_host_error_message(host),
            ),
        }
        if credential.username.trim().is_empty() {
            errors.push(format!("{field}.username"), "Username can't be empty");
        }
        if credential.password_file.trim().is_empty() {
            errors.push(
                format!("{field}.password_file"),
                "password_file cannot be empty",
            );
        }
    }
}

pub(crate) fn invalid_git_host_error_message(host: &str) -> String {
    format!(
        "Host '{host}' is not a bare hostname with an optional port, like github.com or git.example.com:8443"
    )
}
