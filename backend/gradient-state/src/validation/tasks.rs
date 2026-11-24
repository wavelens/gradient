/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::config::{StateAction, StateGitHostConnection, StateTask};
use crate::provisioning::build_trigger_config;
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::input::check_allowed_uri_prefix_has_remote_nix_scheme;
use gradient_types::input::check_task_name;
use gradient_types::triggers::TriggerType;
use gradient_types::{ActionType, RepositoryLocation};
use sea_orm::Iterable;
use std::collections::{HashMap, HashSet};

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let config = lookup.config;
    let mut pinned_trigger_uuids_across_tasks = HashMap::new();
    let mut task_ids_seen = HashSet::new();
    for task in config.tasks.values() {
        errors.record_input_error_at_field(
            format!("tasks.{}.name", task.name),
            check_task_name(&task.name),
        );
        errors.validate_optional_declared_uuid_and_record_duplicate_error(
            format!("tasks.{}.id", task.name),
            "task",
            task.id.as_deref(),
            &mut task_ids_seen,
        );
        if !lookup.project_exists(&task.project) {
            errors.push(
                format!("tasks.{}.project", task.name),
                format!("Project '{}' does not exist", task.project),
            );
        }

        if !lookup.creator_is_blank_or_user_exists(&task.created_by) {
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

        for prefix in &task.extra_allowed_uri_prefixes {
            if let Err(e) = check_allowed_uri_prefix_has_remote_nix_scheme(prefix) {
                errors.push(
                    format!("tasks.{}.extra_allowed_uri_prefixes", task.name),
                    e.to_string(),
                );
            }
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
            validate_task_open_pr_action_git_host_connection_declared(lookup, task, action, errors);
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

        validate_task_triggers(task, &mut pinned_trigger_uuids_across_tasks, errors);
        validate_task_git_host_connection_reference(lookup, task, errors);
    }
}

fn validate_task_triggers(
    task: &StateTask,
    ids: &mut HashMap<uuid::Uuid, String>,
    errors: &mut ErrorCollector,
) {
    for (index, trigger) in task.triggers.iter().flatten().enumerate() {
        let field = |name: &str| format!("tasks.{}.triggers.{}.{}", task.name, index, name);
        match (trigger.trigger_type, &trigger.secret_file) {
            (TriggerType::PostRequest, None) => errors.push(
                field("secret_file"),
                "A post_request trigger needs secret_file.",
            ),
            (TriggerType::PostRequest, Some(_)) | (_, None) => {}
            (_, Some(_)) => errors.push(
                field("secret_file"),
                "Only post_request triggers take secret_file.",
            ),
        }
        if let Err(error) = build_trigger_config(trigger) {
            errors.push(field("config"), error.to_string());
        }

        let Some(id) = trigger.id.as_deref() else {
            continue;
        };
        let Ok(uuid) = id.parse::<uuid::Uuid>() else {
            errors.push(field("id"), format!("Trigger ID '{}' is not a UUID.", id));
            continue;
        };
        match ids.get(&uuid) {
            None => {
                ids.insert(uuid, task.name.clone());
            }
            Some(first_task) if *first_task == task.name => errors.push(
                field("id"),
                format!("Duplicate trigger ID '{}' in task '{}'.", id, task.name),
            ),
            Some(first_task) => errors.push(
                field("id"),
                format!(
                    "Duplicate trigger ID '{}' in task '{}' and '{}'.",
                    id, first_task, task.name
                ),
            ),
        }
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
    {
        errors.push(
            format!("tasks.{}.git_host_connection", task.name),
            format_unknown_connection_name_error_message(name, &task.project),
        );
    }
    if let Some(connection) = connection
        && let Some(address) = connection.address()
        && let Err(reason) = address.check_repository(&task.repository)
    {
        errors.push(
            format!("tasks.{}.git_host_connection", task.name),
            format!(
                "Repository does not match connection '{}': {}",
                connection.name, reason
            ),
        );
    }

    if task.report_status {
        match (name, connection) {
            (None, _) => errors.push(
                format!("tasks.{}.report_status", task.name),
                "A task with report_status needs a git_host_connection field.",
            ),
            (Some(_), Some(connection)) if !connection.reports_status() => errors.push(
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

fn validate_task_open_pr_action_git_host_connection_declared(
    lookup: &EntityLookup,
    task: &StateTask,
    action: &StateAction,
    errors: &mut ErrorCollector,
) {
    if action.action_type != ActionType::OpenPr.as_str() {
        return;
    }
    let Some(name) = action
        .config
        .get("git_host_connection")
        .and_then(|value| value.as_str())
    else {
        return;
    };
    if find_declared_git_host_connection_for_task(lookup, &task.project, name).is_none() {
        errors.push(
            format!(
                "tasks.{}.actions.{}.config.git_host_connection",
                task.name, action.name
            ),
            format_unknown_connection_name_error_message(name, &task.project),
        );
    }
}

fn format_unknown_connection_name_error_message(name: &str, project: &str) -> String {
    format!(
        "Git host connection '{}' does not match any connection owned by project '{}', its owner team, or the instance.",
        name, project
    )
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

#[cfg(test)]
mod validate_task_triggers_tests {
    use super::*;

    #[test]
    fn state_validation_rejects_invalid_trigger_secret_file_and_ids() {
        let duplicate = "0192f0c4-7d4e-7a51-9b2c-3f5e6d7a8b9c";
        let task: StateTask = serde_json::from_value(serde_json::json!({
            "name": "web",
            "project": "acme",
            "display_name": "Web",
            "repository": "https://example.com/acme/web.git",
            "created_by": "alice",
            "triggers": [
                { "type": "post_request" },
                { "type": "polling", "secret_file": "/run/secrets/trigger" },
                { "type": "polling", "id": "not-a-uuid" },
                { "type": "post_request", "secret_file": "/run/secrets/trigger", "id": duplicate },
                { "type": "time", "config": { "cron": "0 2 * * *" }, "id": duplicate },
            ],
        }))
        .unwrap();
        let mut errors = ErrorCollector::new();

        validate_task_triggers(&task, &mut HashMap::new(), &mut errors);

        let fields: Vec<String> = errors.into_errors().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "tasks.web.triggers.0.secret_file",
                "tasks.web.triggers.1.secret_file",
                "tasks.web.triggers.2.id",
                "tasks.web.triggers.4.id",
            ]
        );
    }

    #[test]
    fn reporter_pull_request_trigger_branches_string_reports_single_config_validation_error() {
        let task: StateTask = serde_json::from_value(serde_json::json!({
            "name": "web",
            "project": "acme",
            "display_name": "Web",
            "repository": "https://example.com/acme/web.git",
            "created_by": "alice",
            "triggers": [
                { "type": "reporter_pull_request", "config": { "branches": ["main"] } },
                { "type": "reporter_pull_request", "config": { "branches": "*" } },
            ],
        }))
        .unwrap();
        let mut errors = ErrorCollector::new();

        validate_task_triggers(&task, &mut HashMap::new(), &mut errors);

        let errors = errors.into_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "tasks.web.triggers.1.config");
        assert_eq!(
            errors[0].message.matches("expected a sequence").count(),
            1,
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn duplicate_pinned_trigger_id_across_tasks_names_both_tasks() {
        let task_with_pinned_trigger = |name: &str| -> StateTask {
            serde_json::from_value(serde_json::json!({
                "name": name,
                "project": "acme",
                "display_name": name,
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "triggers": [{ "type": "polling", "id": uuid::Uuid::from_u128(7).to_string() }],
            }))
            .unwrap()
        };
        let mut ids = HashMap::new();
        let mut errors = ErrorCollector::new();

        validate_task_triggers(&task_with_pinned_trigger("web"), &mut ids, &mut errors);
        validate_task_triggers(&task_with_pinned_trigger("docs"), &mut ids, &mut errors);

        let errors = errors.into_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "tasks.docs.triggers.0.id");
        assert!(errors[0].message.contains("'web'"), "{}", errors[0].message);
        assert!(
            errors[0].message.contains("'docs'"),
            "{}",
            errors[0].message
        );
    }
}
