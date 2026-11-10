/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::task::{
    FlakeInputCommands, TaskActionCommands, TaskRepositoryCommands, TaskTriggerCommands,
};
use crate::input::{client_from_config, selected_task};
use crate::output::{Color, ExitKind, Output};
use connector::Client;
use connector::api::{
    ActionConfig, CreateActionRequest, CreateFlakeInputOverrideRequest, CreateTriggerRequest,
    FlakeInputOverrideId, InstallDeployKeyRequest, InstallWebhookRequest, IntegrationId,
    TaskActionId, TaskRepositoryResponse, TriggerConfig, UpdateActionRequest, UpdateTriggerRequest,
};
use serde::de::DeserializeOwned;

fn parse_json_configuration_from_command_line<T: DeserializeOwned>(config: &str, out: Output) -> T {
    serde_json::from_str(config).unwrap_or_else(|error| {
        out.err(
            ExitKind::Usage,
            format!("Invalid JSON in --config: {error}"),
        )
    })
}

fn integration_uuid_from_uuid_or_name_using_task_repository_integrations(
    reference: &str,
    repository: &TaskRepositoryResponse,
    out: Output,
) -> IntegrationId {
    if let Ok(id) = reference.parse() {
        return id;
    }
    repository
        .integrations
        .iter()
        .find(|integration| integration.name == reference)
        .map(|integration| integration.id.clone())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!("This project has no integration named '{reference}'."),
            )
        })
}

async fn find_task_action_uuid_by_reference(
    client: &Client,
    project: &str,
    task: &str,
    reference: &str,
    out: Output,
) -> String {
    if reference.parse::<TaskActionId>().is_ok() {
        return reference.to_owned();
    }
    let actions = client
        .task_event_actions()
        .list(project, task)
        .await
        .unwrap_or_else(|e| out.fail(e));
    actions
        .into_iter()
        .find(|action| action.name == reference)
        .map(|action| action.id.to_string())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!("This task has no action named '{reference}'."),
            )
        })
}

async fn find_override_uuid_by_input_name_or_uuid(
    client: &Client,
    project: &str,
    task: &str,
    reference: &str,
    out: Output,
) -> String {
    if reference.parse::<FlakeInputOverrideId>().is_ok() {
        return reference.to_owned();
    }
    let overrides = client
        .flake_inputs()
        .list(project, task)
        .await
        .unwrap_or_else(|e| out.fail(e));
    overrides
        .into_iter()
        .find(|flake_input| flake_input.input_name == reference)
        .map(|flake_input| flake_input.id.to_string())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!(
                    "This task doesn't have an override for a flake input named '{reference}'."
                ),
            )
        })
}

fn print_repository(repository: &TaskRepositoryResponse, out: Output) {
    let integration_name = |id: &IntegrationId| {
        repository
            .integrations
            .iter()
            .find(|integration| &integration.id == id)
            .map_or_else(|| id.to_string(), |integration| integration.name.clone())
    };
    let state = |active: bool| if active { "on" } else { "off" };

    out.human(format!("Repository: {}", repository.repository));
    if let Some(report) = &repository.report_status {
        out.human(format!(
            "Status Report: {} ({})",
            state(report.active),
            integration_name(&report.integration_id),
        ));
    }
    if let Some(deploy_key) = &repository.deploy_key {
        out.human(format!("Deploy key: {deploy_key}"));
    }
    if !repository.integrations.is_empty() {
        out.human("Integration:");
        for integration in &repository.integrations {
            out.human(format!(
                "  {} ({}, {}): {}",
                integration.name, integration.kind, integration.git_host_type, integration.id
            ));
        }
    }
}

pub async fn handle_task_repository_subcommand(cmd: TaskRepositoryCommands, out: Output) {
    let (project, task) = selected_task(out);
    let client = client_from_config(out);
    let repository = client
        .task_repository()
        .get(&project, &task)
        .await
        .unwrap_or_else(|e| out.fail(e));

    match cmd {
        TaskRepositoryCommands::Show => {
            out.ok(&repository);
            print_repository(&repository, out);
        }

        TaskRepositoryCommands::InstallDeployKey {
            outbound_integration,
        } => {
            let outbound_integration = outbound_integration
                .map(|reference| {
                    integration_uuid_from_uuid_or_name_using_task_repository_integrations(
                        &reference,
                        &repository,
                        out,
                    )
                })
                .or_else(|| {
                    repository
                        .report_status
                        .as_ref()
                        .map(|report| report.integration_id.clone())
                })
                .unwrap_or_else(|| {
                    out.err(
                        ExitKind::Usage,
                        "This task doesn't report commit status. Set `--outbound-integration`.",
                    )
                });

            let path = client
                .task_repository()
                .add_project_ssh_public_key_as_read_only_deploy_key(
                    &project,
                    &task,
                    &InstallDeployKeyRequest {
                        outbound_integration,
                    },
                )
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"installed": true, "repository": path}));
            out.human(format!("Installed deploy key at {path}."));
        }

        TaskRepositoryCommands::InstallWebhook {
            inbound_integration,
            outbound_integration,
        } => {
            let resolve = |reference: String| {
                integration_uuid_from_uuid_or_name_using_task_repository_integrations(
                    &reference,
                    &repository,
                    out,
                )
            };
            let inbound_integration = inbound_integration
                .map(resolve)
                .or_else(|| {
                    repository
                        .integrations
                        .iter()
                        .find(|integration| {
                            integration.kind == "inbound" && integration.serves_repository
                        })
                        .map(|integration| integration.id.clone())
                })
                .unwrap_or_else(|| {
                    out.err(
                        ExitKind::Usage,
                        "No webhook receivers exist for this repository. Specify `--inbound-integration`.",
                    )
                });
            let outbound_integration = outbound_integration
                .map(resolve)
                .or_else(|| {
                    repository
                        .report_status
                        .as_ref()
                        .map(|report| report.integration_id.clone())
                })
                .unwrap_or_else(|| {
                    out.err(
                        ExitKind::Usage,
                        "This task doesn't report commit status. Set `--outbound-integration`.",
                    )
                });

            let path = client
                .task_repository()
                .create_repository_webhook(
                    &project,
                    &task,
                    &InstallWebhookRequest {
                        inbound_integration,
                        outbound_integration,
                    },
                )
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"installed": true, "repository": path}));
            out.human(format!("Installed the webhook at {path}."));
        }
    }
}

pub async fn run_gradient_task_action_subcommand(cmd: TaskActionCommands, out: Output) {
    let (project, task) = selected_task(out);
    let client = client_from_config(out);
    let actions = client.task_event_actions();

    match cmd {
        TaskActionCommands::List => {
            let list = actions
                .list(&project, &task)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&list);
            if list.is_empty() {
                out.human("No actions.");
            }
            for action in &list {
                let color = if action.active {
                    Color::Plain
                } else {
                    Color::White
                };
                out.human(out.paint(
                    &format!("{}: {} [{}]", action.name, action.id, action.action_type),
                    color,
                ));
            }
        }

        TaskActionCommands::Show { action } => {
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let action = actions
                .get(&project, &task, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&action);
            out.human(format!("{} ({})", action.name, action.id));
            out.human(format!("{}, active: {}", action.action_type, action.active));
            out.human(action.events.join(", "));
            out.human(serde_json::to_string_pretty(&action.config).unwrap_or_default());
        }

        TaskActionCommands::Create {
            name,
            config,
            events,
            inactive,
        } => {
            let body = CreateActionRequest {
                name,
                config: parse_json_configuration_from_command_line::<ActionConfig>(&config, out),
                events,
                active: Some(!inactive),
            };
            let created = actions
                .create(&project, &task, &body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&created);
            out.human("Created action.");
            if let Some(token) = &created.token {
                out.human(format!("Web request token (shown only once): {token}"));
            }
        }

        TaskActionCommands::Edit {
            action,
            name,
            config,
            events,
            active,
        } => {
            if name.is_none() && config.is_none() && events.is_none() && active.is_none() {
                out.err(
                    ExitKind::Usage,
                    "Nothing to change. Specify at least one flag.",
                );
            }
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let body = UpdateActionRequest {
                name,
                config: config.map(|config| {
                    parse_json_configuration_from_command_line::<ActionConfig>(&config, out)
                }),
                events,
                active,
            };
            let updated = actions
                .update(&project, &task, &id, &body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&updated);
            out.human("Updated action.");
        }

        TaskActionCommands::Delete { action } => {
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let deleted = actions
                .delete(&project, &task, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deleted);
            out.human("Removed the action.");
        }

        TaskActionCommands::Deliveries {
            action,
            limit,
            offset,
        } => {
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let deliveries = actions
                .list_delivery_history(&project, &task, &id, limit, offset)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deliveries);
            if deliveries.is_empty() {
                out.human("No deliveries.");
            }
            for delivery in &deliveries {
                let result = delivery
                    .response_status
                    .map(|status| status.to_string())
                    .or_else(|| delivery.error_message.clone())
                    .unwrap_or_default();
                let color = if delivery.success {
                    Color::Green
                } else {
                    Color::Red
                };
                out.human(format!(
                    "{} {} {} {}",
                    delivery.delivered_at,
                    delivery.event,
                    out.paint(&result, color),
                    delivery.id
                ));
            }
        }

        TaskActionCommands::Delivery { action, delivery } => {
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let delivery = actions
                .get_task_action_delivery_with_bodies(&project, &task, &id, &delivery)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&delivery);
            out.human(format!(
                "{} {} {} {}ms",
                delivery.delivered_at,
                delivery.event,
                delivery
                    .response_status
                    .map(|status| status.to_string())
                    .unwrap_or_default(),
                delivery.duration_ms
            ));
            if let Some(error) = &delivery.error_message {
                out.human(error);
            }
            out.human("");
            out.human(&delivery.request_body);
            if let Some(response) = &delivery.response_body {
                out.human("");
                out.human(response);
            }
        }

        TaskActionCommands::Test { action } => {
            let id =
                find_task_action_uuid_by_reference(&client, &project, &task, &action, out).await;
            let result = actions
                .test(&project, &task, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&result);
            out.human("Action executed.");
        }
    }
}

pub async fn run_flake_input_subcommand(cmd: FlakeInputCommands, out: Output) {
    let (project, task) = selected_task(out);
    let client = client_from_config(out);
    let flake_inputs = client.flake_inputs();
    let print = |flake_input: &connector::api::FlakeInputOverrideOut| {
        out.human(format!(
            "{}: {} ({})",
            flake_input.input_name,
            flake_input.url.as_deref().unwrap_or("-"),
            flake_input.id
        ));
    };

    match cmd {
        FlakeInputCommands::List => {
            let list = flake_inputs
                .list(&project, &task)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&list);
            if list.is_empty() {
                out.human("No flake input overrides.");
            }
            list.iter().for_each(print);
        }

        FlakeInputCommands::Show { input } => {
            let id =
                find_override_uuid_by_input_name_or_uuid(&client, &project, &task, &input, out)
                    .await;
            let flake_input = flake_inputs
                .get(&project, &task, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&flake_input);
            print(&flake_input);
        }

        FlakeInputCommands::Create { input_name, url } => {
            let created = flake_inputs
                .create(
                    &project,
                    &task,
                    &CreateFlakeInputOverrideRequest { input_name, url },
                )
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&created);
            out.human("Created an override for the flake input.");
        }

        FlakeInputCommands::Edit {
            input,
            new_name,
            url,
            clear_url,
        } => {
            if new_name.is_none() && url.is_none() && !clear_url {
                out.err(
                    ExitKind::Usage,
                    "Nothing to change. Specify at least one flag.",
                );
            }
            let id =
                find_override_uuid_by_input_name_or_uuid(&client, &project, &task, &input, out)
                    .await;
            let url = if clear_url {
                Some(None)
            } else {
                url.as_deref().map(Some)
            };
            let updated = flake_inputs
                .update(&project, &task, &id, new_name.as_deref(), url)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&updated);
            out.human("Updated the overrides for flake inputs.");
        }

        FlakeInputCommands::Delete { input } => {
            let id =
                find_override_uuid_by_input_name_or_uuid(&client, &project, &task, &input, out)
                    .await;
            let deleted = flake_inputs
                .delete(&project, &task, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deleted);
            out.human("Removed the override for flake inputs.");
        }
    }
}

pub async fn run_task_trigger_subcommand(cmd: TaskTriggerCommands, out: Output) {
    let (project, task) = selected_task(out);
    let client = client_from_config(out);
    let tasks = client.tasks();
    let print = |trigger: &connector::api::TriggerOut| {
        let default = if trigger.is_default { " (default)" } else { "" };
        let color = if trigger.active {
            Color::Plain
        } else {
            Color::White
        };
        out.human(out.paint(
            &format!(
                "{}: {} {}{default}",
                trigger.id, trigger.type_, trigger.config
            ),
            color,
        ));
    };

    match cmd {
        TaskTriggerCommands::List => {
            let list = tasks
                .triggers(&project, &task)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&list);
            if list.is_empty() {
                out.human("There are no triggers.");
            }
            list.iter().for_each(print);
        }

        TaskTriggerCommands::Show { trigger } => {
            let trigger = tasks
                .get_trigger(&project, &task, &trigger)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&trigger);
            print(&trigger);
        }

        TaskTriggerCommands::Create { config, inactive } => {
            let body = CreateTriggerRequest {
                config: parse_json_configuration_from_command_line::<TriggerConfig>(&config, out),
                active: Some(!inactive),
            };
            let id = tasks
                .create_trigger(&project, &task, body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"created": true, "id": id}));
            out.human("Created trigger.");
        }

        TaskTriggerCommands::Edit {
            trigger,
            config,
            active,
        } => {
            if config.is_none() && active.is_none() {
                out.err(
                    ExitKind::Usage,
                    "Nothing to change. Specify at least one flag.",
                );
            }
            let body = UpdateTriggerRequest {
                config: config.map(|config| {
                    parse_json_configuration_from_command_line::<TriggerConfig>(&config, out)
                }),
                active,
            };
            tasks
                .update_trigger(&project, &task, &trigger, body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"updated": true}));
            out.human("Trigger update succeeded.");
        }

        TaskTriggerCommands::Delete { trigger } => {
            tasks
                .delete_trigger(&project, &task, &trigger)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"deleted": true}));
            out.human("Deleted trigger.");
        }

        TaskTriggerCommands::Test { trigger } => {
            let outcome = tasks
                .test_trigger(&project, &task, &trigger)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&serde_json::json!({"outcome": outcome}));
            out.human(format!("Trigger fired: {outcome}"));
        }

        TaskTriggerCommands::Default { trigger } => {
            let trigger = tasks
                .set_default_trigger(&project, &task, &trigger)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&trigger);
            out.human("The trigger is the default.");
        }
    }
}
