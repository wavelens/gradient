/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::config::*;
use crate::input::{client_from_config, handle_input};
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use colored::*;
use connector::projects::{MakeProjectRequest, PatchProjectRequest};

#[derive(Subcommand, Debug)]
pub enum Commands {
    Select {
        #[arg(add = ArgValueCompleter::new(completion::complete_projects))]
        project: String,
    },
    Show,
    Log,
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
        #[arg(short, long)]
        repository: Option<String>,
        #[arg(short = 'w', long)]
        wildcard: Option<String>,
    },
    List,
    Edit {
        #[arg(short, long)]
        new_name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
        #[arg(short, long)]
        repository: Option<String>,
        #[arg(short = 'w', long)]
        wildcard: Option<String>,
    },
    Delete,
    Evaluate,
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Select { project } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            set_get_value(
                ConfigKey::SelectedProject,
                Some(format!("{}/{}", organization, project)),
                true,
            )
            .unwrap();
            out.human("Project selected in current Organization.");
        }

        Commands::Show => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split('/').collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => out.err(ExitKind::Usage, "Project is required for command."),
                };

            let client = client_from_config(out);

            let details = match client.projects().details(&organization, &project).await {
                Ok(d) => d,
                Err(e) => out.err(to_exit_kind(&e), e),
            };

            out.ok(&details);
            out.human("===== Project =====");
            out.human(format!("Name: {}", details.name));
            out.human(format!("Description: {}", details.description));
            out.human(format!("Repository: {}", details.repository));
            out.human(format!("Wildcard: {}", details.wildcard));
            out.human("");

            if details.last_evaluations.is_empty() {
                out.human("No last evaluation.");
                return;
            }

            let eval_summary = &details.last_evaluations[0];
            let eval = match client.evals().get(&eval_summary.id).await {
                Ok(e) => e,
                Err(e) => out.err(to_exit_kind(&e), e),
            };

            out.human("===== Evaluation =====");
            out.human(format!("ID: {}", eval.id));
            out.human(format!("Status: {}", eval.status));
            out.human(format!("Commit: {}", eval.commit));
            if let Some(error) = &eval.error {
                out.human(format!("Error: {}", error));
            }
            out.human("");

            let builds = match client.evals().builds(&eval.id).await {
                Ok(b) => b,
                Err(e) => out.err(to_exit_kind(&e), e),
            };

            if builds.builds.is_empty() {
                out.human("No builds.");
                return;
            }

            out.human("===== Building =====");
            for build in &builds.builds {
                let colored_name = match build.status.as_str() {
                    "Completed" => build.name.green(),
                    "Building" | "Running" => build.name.yellow(),
                    "Queued" | "Pending" => build.name.white(),
                    "Failed" | "Error" => build.name.red(),
                    _ => build.name.normal(),
                };
                out.human(format!("{}", colored_name));
            }
            out.human("");

            if eval.status != "Aborted" {
                out.human("===== Log =====");
                crate::commands::logstream::stream_eval_logs(&client, &eval.id, out).await;
            }
        }

        Commands::Log => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split('/').collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => out.err(ExitKind::Usage, "Project is required for command."),
                };

            let client = client_from_config(out);
            let details = match client.projects().details(&organization, &project).await {
                Ok(d) => d,
                Err(e) => out.err(to_exit_kind(&e), e),
            };
            let Some(latest) = details.last_evaluations.first() else {
                out.err(ExitKind::Api, "Project has no evaluations yet.");
            };
            crate::commands::logstream::stream_eval_logs(&client, &latest.id, out).await;
        }

        Commands::Create {
            name,
            display_name,
            description,
            repository,
            wildcard,
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
                ("Repository", repository),
                ("Wildcard", wildcard),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let client = client_from_config(out);
            match client
                .projects()
                .create(
                    &organization,
                    MakeProjectRequest {
                        name: name.clone(),
                        display_name: input.get("Display Name").unwrap().clone(),
                        description: input.get("Description").unwrap().clone(),
                        repository: input.get("Repository").unwrap().clone(),
                        wildcard: input.get("Wildcard").unwrap().clone(),
                    },
                )
                .await
            {
                Ok(_) => {
                    set_get_value(
                        ConfigKey::SelectedProject,
                        Some(format!("{}/{}", organization, name)),
                        true,
                    )
                    .unwrap();
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Project created.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);
            match client.projects().list(&organization).await {
                Ok(res) => {
                    out.ok(&res);
                    if res.items.is_empty() {
                        out.human("You have no projects.");
                    } else {
                        for project in res.items {
                            out.human(format!("{}: {}", project.name, project.id));
                        }
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Edit {
            new_name,
            display_name,
            description,
            repository,
            wildcard,
        } => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split('/').collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => out.err(ExitKind::Usage, "Project is required for command."),
                };

            let client = client_from_config(out);
            let current = match client.projects().details(&organization, &project).await {
                Ok(d) => d,
                Err(e) => out.err(to_exit_kind(&e), e),
            };

            let input_fields = [
                ("Name", Some(new_name.unwrap_or(current.name))),
                (
                    "Display Name",
                    Some(display_name.unwrap_or(current.display_name)),
                ),
                (
                    "Description",
                    Some(description.unwrap_or(current.description)),
                ),
                ("Repository", Some(repository.unwrap_or(current.repository))),
                ("Wildcard", Some(wildcard.unwrap_or(current.wildcard))),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false);

            match client
                .projects()
                .update(
                    &organization,
                    &project,
                    PatchProjectRequest {
                        name: input.get("Name").cloned(),
                        display_name: input.get("Display Name").cloned(),
                        description: input.get("Description").cloned(),
                        repository: input.get("Repository").cloned(),
                        wildcard: input.get("Wildcard").cloned(),
                    },
                )
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Project updated.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Delete => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split('/').collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => out.err(ExitKind::Usage, "Project is required for command."),
                };

            let client = client_from_config(out);
            match client.projects().delete(&organization, &project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Project deleted.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Evaluate => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split('/').collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => out.err(ExitKind::Usage, "Project is required for command."),
                };

            let client = client_from_config(out);
            match client.projects().evaluate(&organization, &project).await {
                Ok(eval_id) => {
                    out.ok(&serde_json::json!({"evaluation_id": eval_id}));
                    out.human("Project evaluation started.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
    }
}
