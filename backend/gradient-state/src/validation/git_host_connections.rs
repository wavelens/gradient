/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_types::git_host::{GitHostConnectionOwner, GitHostType};
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let mut seen: HashSet<(GitHostConnectionOwner, &str)> = HashSet::new();
    for (label, connection) in &lookup.config.git_host_connections {
        let field = |key: &str| format!("git_host_connections.{label}.{key}");

        let owner = match connection.owner.parse::<GitHostConnectionOwner>() {
            Ok(owner) => Some(owner),
            Err(message) => {
                errors.push(field("owner"), message);
                None
            }
        };
        match &owner {
            Some(GitHostConnectionOwner::Project { name }) if !lookup.project_exists(name) => {
                errors.push(field("owner"), format!("Project '{}' does not exist", name));
            }
            Some(GitHostConnectionOwner::Team { name }) if !lookup.team_exists(name) => {
                errors.push(field("owner"), format!("Team '{}' does not exist", name));
            }
            _ => {}
        }
        if let Some(owner) = &owner
            && !seen.insert((owner.clone(), connection.name.as_str()))
        {
            errors.push(
                field("name"),
                format!(
                    "Connection name '{}' to the git host is a duplicate for owner '{}'.",
                    connection.name, owner
                ),
            );
        }

        if !lookup.user_exists(&connection.created_by) {
            errors.push(
                field("created_by"),
                format!("User '{}' does not exist", connection.created_by),
            );
        }

        let git_host_type = GitHostType::from_path_segment(&connection.git_host_type);
        if git_host_type.is_none() {
            errors.push(
                field("git_host_type"),
                format!(
                    "Invalid git_host_type '{}': expected gitea/forgejo/gitlab/github",
                    connection.git_host_type
                ),
            );
        }

        let is_github = git_host_type == Some(GitHostType::GitHub);
        if is_github && connection.installation_id.is_none_or(|id| id <= 0) {
            errors.push(
                field("installation_id"),
                "git_host_type 'github' requires a positive installation_id",
            );
        }
        if connection.installation_id.is_some()
            && !(is_github && owner == Some(GitHostConnectionOwner::Instance))
        {
            errors.push(
                field("installation_id"),
                "Set owner to 'instance' and git_host_type to 'github' for installation_id.",
            );
        }
    }
}
