/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use colored::*;
use connector::*;
use std::process::exit;

#[derive(Subcommand, Debug)]
pub enum Commands {
    Select {
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
                None => {
                    out.err(ExitKind::Usage, "Organization is required for command.");
                }
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
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => {
                        out.err(ExitKind::Usage, "Project is required for command.");
                    }
                };

            let project = projects::get_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if project.error {
                out.err(ExitKind::Api, "Failed to show project.");
            }

            out.human("===== Project =====");
            out.human(format!("Name: {}", project.message.name));
            out.human(format!("Description: {}", project.message.description));
            out.human(format!("Repository: {}", project.message.repository));
            out.human(format!("Wildcard: {}", project.message.wildcard));
            out.human(format!("Organization ID: {}", project.message.organization));
            out.human("");

            if project.message.last_evaluation.is_none() {
                out.human("No last evaluation.");
                exit(0);
            }

            let evaluation = evals::get_evaluation(
                get_request_config(load_config()).unwrap(),
                project.message.last_evaluation.unwrap(),
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if evaluation.error {
                out.err(ExitKind::Api, "Failed to show evaluation.");
            }

            out.human("===== Evaluation =====");
            out.human(format!("ID: {}", evaluation.message.id));
            out.human(format!("Status: {}", evaluation.message.status));
            out.human(format!("Commit: {}", evaluation.message.commit));
            if let Some(error) = &evaluation.message.error {
                out.human(format!("Error: {}", error));
            }
            out.human("");

            let builds = evals::get_evaluation_builds(
                get_request_config(load_config()).unwrap(),
                evaluation.message.id.clone(),
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if builds.error {
                out.err(ExitKind::Api, "Failed to get builds.");
            }

            if builds.message.builds.is_empty() {
                out.human("No builds.");
                exit(0);
            }

            out.human("===== Building =====");
            for build in builds.message.builds.iter() {
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

            if evaluation.message.status != "Aborted" {
                out.human("===== Log =====");
                evals::post_evaluation_builds(
                    get_request_config(load_config()).unwrap(),
                    evaluation.message.id,
                )
                .await
                .map_err(|e| {
                    out.progress(format!("{}", e));
                    exit(1);
                })
                .unwrap();
            }
        }

        Commands::Log => {
            todo!();
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
                _ => {
                    out.err(ExitKind::Usage, "Organization is required for command.");
                }
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

            let res = projects::put(
                get_request_config(load_config()).unwrap(),
                organization.clone(),
                name.clone(),
                input.get("Display Name").unwrap().clone(),
                input.get("Description").unwrap().clone(),
                input.get("Repository").unwrap().clone(),
                input.get("Wildcard").unwrap().clone(),
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, format!("Project creation failed: {}", res.message));
            }

            set_get_value(
                ConfigKey::SelectedProject,
                Some(format!("{}/{}", organization, name)),
                true,
            )
            .unwrap();
            out.human("Project created.");
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    out.err(ExitKind::Usage, "Organization is required for command.");
                }
            };

            let res = projects::get(get_request_config(load_config()).unwrap(), organization)
                .await
                .map_err(|e| {
                    out.progress(format!("{}", e));
                    exit(1);
                })
                .unwrap();

            if res.message.items.is_empty() {
                out.human("You have no projects.");
            } else {
                for project in res.message.items {
                    out.human(format!("{}: {}", project.name, project.id));
                }
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
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => {
                        out.err(ExitKind::Usage, "Project is required for command.");
                    }
                };

            let current_project = projects::get_project(
                get_request_config(load_config()).unwrap(),
                organization.clone(),
                project.clone(),
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap()
            .message;

            let input_fields = [
                ("Name", Some(new_name.unwrap_or(current_project.name))),
                (
                    "Display Name",
                    Some(display_name.unwrap_or(current_project.display_name)),
                ),
                (
                    "Description",
                    Some(description.unwrap_or(current_project.description)),
                ),
                (
                    "Repository",
                    Some(repository.unwrap_or(current_project.repository)),
                ),
                (
                    "Wildcard",
                    Some(wildcard.unwrap_or(current_project.wildcard)),
                ),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false);

            let res = projects::patch_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
                input.get("New Name").cloned(),
                input.get("Display Name").cloned(),
                input.get("Description").cloned(),
                input.get("Repository").cloned(),
                input.get("Wildcard").cloned(),
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, format!("Project creation failed: {}", res.message));
            }

            out.human("Project updated.");
        }

        Commands::Delete => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => {
                        out.err(ExitKind::Usage, "Project is required for command.");
                    }
                };

            let res = projects::delete_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, "Failed to delete project.");
            }

            out.human("Project deleted.");
        }

        Commands::Evaluate => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    _ => {
                        out.err(ExitKind::Usage, "Project is required for command.");
                    }
                };

            let res = projects::post_project_evaluate(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, format!("Failed to start project evaluation: {}", res.message));
            }

            out.human("Project evaluation started.");
        }
    }
}
