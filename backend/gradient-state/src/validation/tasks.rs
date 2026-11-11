/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::config::{StateGitHostConnection, StateTask};
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::triggers::TriggerType;
use gradient_types::{ActionType, RepositoryLocation};
use sea_orm::Iterable;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    for task in config.tasks.values() {
        if !lookup.project_exists(&task.project) {
            errors.push(
                format!("tasks.{}.project", task.name),
                format!("Project '{}' does not exist", task.project),
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

        if let Err(e) = RepositoryLocation::check(&task.repository) {
            errors.push(format!("tasks.{}.repository", task.name), e.to_string());
        }

        if task.keep_evaluations < 1 {
            errors.push(
                format!("tasks.{}.keep_evaluations", task.name),
                "keep_evaluations must be at least 1",
            );
        }

        let mut action_names: HashSet<&str> = HashSet::new();
        for action in &task.actions {
            if ActionType::from_name(&action.action_type).is_none() {
                let names: Vec<&str> = ActionType::iter().map(ActionType::as_str).collect();
                errors.push(
                    format!("tasks.{}.actions.{}.type", task.name, action.name),
                    format!(
                        "Invalid action type '{}': expected {}",
                        action.action_type,
                        names.join("/")
                    ),
                );
            }

            if action.action_type == ActionType::OpenPr.as_str() && !action.events.is_empty() {
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

        let default_triggers = task.triggers.iter().flatten().filter(|t| t.is_default);
        if default_triggers.count() > 1 {
            errors.push(
                format!("tasks.{}.triggers", task.name),
                "at most one trigger can have `is_default` set",
            );
        }

        validate_task_git_host_connection_reference(lookup, task, errors);
    }
}

fn validate_task_git_host_connection_reference(
    lookup: &EntityLookup,
    task: &StateTask,
    errors: &mut ErrorCollector,
) {
    let name = task.git_host_connection.as_deref();
    let connection = name
        .and_then(|name| find_declared_git_host_connection_for_task(lookup, &task.project, name));
    if let Some(name) = name
        && connection.is_none()
        && !is_auto_created_github_app_connection_name(name)
    {
        errors.push(
            format!("tasks.{}.git_host_connection", task.name),
            format!(
                "Git host connection '{}' does not match any connection owned by project '{}', its owner team, or the instance.",
                name, task.project
            ),
        );
    }

    if task.report_status {
        match (name, connection) {
            (None, _) => errors.push(
                format!("tasks.{}.report_status", task.name),
                "A task with report_status needs a git_host_connection field.",
            ),
            (Some(_), Some(connection)) if !connection.reports_status => errors.push(
                format!("tasks.{}.report_status", task.name),
                format!(
                    "report_status needs a connection to a Git host with reports_status set. The connection '{}' has reports_status = false.",
                    connection.name
                ),
            ),
            _ => {}
        }
    }

    let has_reporter_trigger = task.triggers.iter().flatten().any(|trigger| {
        matches!(
            trigger.trigger_type,
            TriggerType::ReporterPush | TriggerType::ReporterPullRequest
        )
    });
    if has_reporter_trigger {
        match (name, connection) {
            (None, _) => errors.push(
                format!("tasks.{}.triggers", task.name),
                "The `reporter_push` and `reporter_pull_request` triggers require the task's `git_host_connection`.",
            ),
            (Some(_), Some(connection)) if !connection.receives_events => errors.push(
                format!("tasks.{}.triggers", task.name),
                format!(
                    "The reporter_push/reporter_pull_request triggers need a connection to a git host with receives_events set. '{}' does not have that setting.",
                    connection.name
                ),
            ),
            _ => {}
        }
    }
}

fn find_declared_git_host_connection_for_task<'a>(
    lookup: &EntityLookup<'a>,
    project: &str,
    name: &str,
) -> Option<&'a StateGitHostConnection> {
    let config = lookup.config;
    let owner_team = config
        .projects
        .get(project)
        .and_then(|project| project.owner_team.clone());
    [
        Some(GitHostConnectionOwner::Project {
            name: project.to_owned(),
        }),
        owner_team.map(|name| GitHostConnectionOwner::Team { name }),
        Some(GitHostConnectionOwner::Instance),
    ]
    .into_iter()
    .flatten()
    .find_map(|owner| {
        config.git_host_connections.values().find(|connection| {
            connection.name == name && connection.owner.parse().ok().as_ref() == Some(&owner)
        })
    })
}

fn is_auto_created_github_app_connection_name(name: &str) -> bool {
    name == "github" || name.starts_with("github-")
}
