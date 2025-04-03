/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{arg, Subcommand};
use std::process::exit;
use connector::*;
use crate::config::*;
use crate::input::*;

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
        evaluation_wildcard: Option<String>,
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
        evaluation_wildcard: Option<String>,
    },
    Delete,
}

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Select { project } => {
            let organization =
                match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                    Some(id) => id,
                    None => {
                        eprintln!("Organization is required for command.");
                        exit(1);
                    }
                };

            set_get_value(
                ConfigKey::SelectedProject,
                Some(format!("{}/{}", organization, project)),
                true,
            )
            .unwrap();
            println!("Project selected in current Organization.");
        }

        Commands::Show => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    None => {
                        eprintln!("Project is required for command.");
                        exit(1);
                    }
                };

            let project = projects::get_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if project.error {
                eprintln!("Failed to show project.");
                exit(1);
            }

            println!("===== Project =====");
            println!("Name: {}", project.message.name);
            println!("Description: {}", project.message.description);
            println!("Repository: {}", project.message.repository);
            println!(
                "Evaluation Wildcard: {}",
                project.message.evaluation_wildcard
            );
            println!("Organization ID: {}", project.message.organization);
            println!("Last Check At: {}", project.message.last_check_at);
            println!();

            if project.message.last_evaluation.is_none() {
                println!("No last evaluation.");
                exit(0);
            }

            let evaluation = evals::get_evaluation(
                get_request_config(load_config()).unwrap(),
                project.message.last_evaluation.unwrap(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if evaluation.error {
                eprintln!("Failed to show evaluation.");
                exit(1);
            }

            println!("===== Evaluation =====");
            println!("ID: {}", evaluation.message.id);
            println!("Status: {}", evaluation.message.status);
            println!("Commit: {}", evaluation.message.commit);
            println!();

            let builds = evals::get_evaluation_builds(
                get_request_config(load_config()).unwrap(),
                evaluation.message.id.clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if builds.error {
                eprintln!("Failed to get builds.");
                exit(1);
            }

            if builds.message.is_empty() {
                println!("No builds.");
                exit(0);
            }

            println!("===== Building =====");
            for build in builds.message.clone() {
                println!("{}", build.name);
            }
            println!();

            println!("===== Log =====");
            evals::connect_evaluation(
                get_request_config(load_config()).unwrap(),
                evaluation.message.id,
            )
            .await
            .unwrap();
        }

        Commands::Log => {
            todo!();
        }

        Commands::Create {
            name,
            display_name,
            description,
            repository,
            evaluation_wildcard,
        } => {
            let organization =
                match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                    Some(id) => id,
                    None => {
                        eprintln!("Organization is required for command.");
                        exit(1);
                    }
                };

            let input_fields = vec![
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
                ("Repository", repository),
                ("Evaluation Wildcard", evaluation_wildcard),
            ].iter().map(|(k, v)| {
                (k.to_string(), v.clone())
            }).collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let res = projects::put(
                get_request_config(load_config()).unwrap(),
                organization.clone(),
                name.clone(),
                input.get("Display Name").unwrap().clone(),
                input.get("Description").unwrap().clone(),
                input.get("Repository").unwrap().clone(),
                input.get("Evaluation Wildcard").unwrap().clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Project creation failed: {}", res.message);
                exit(1);
            }

            set_get_value(
                ConfigKey::SelectedProject,
                Some(format!("{}/{}", organization, name)),
                true,
            )
            .unwrap();
            println!("Project created.");
        }

        Commands::List => {
            let organization =
                match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                    Some(id) => id,
                    None => {
                        eprintln!("Organization is required for command.");
                        exit(1);
                    }
                };

            let res = projects::get(
                get_request_config(load_config()).unwrap(),
                organization,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.message.is_empty() {
                println!("You have no projects.");
            } else {
                for project in res.message {
                    println!("{}: {}", project.name, project.id);
                }
            }
        }

        Commands::Edit {
            new_name,
            display_name,
            description,
            repository,
            evaluation_wildcard,
        } => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    None => {
                        eprintln!("Project is required for command.");
                        exit(1);
                    }
                };

            let current_project =
                projects::get_project(
                    get_request_config(load_config()).unwrap(),
                    organization.clone(),
                    project.clone(),
                )
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap()
                .message;

            let input_fields = vec![
                ("Name", Some(new_name.unwrap_or(current_project.name))),
                ("Display Name", Some(display_name.unwrap_or(current_project.display_name))),
                ("Description", Some(description.unwrap_or(current_project.description))),
                ("Repository", Some(repository.unwrap_or(current_project.repository))),
                ("Evaluation Wildcard", Some(evaluation_wildcard.unwrap_or(current_project.evaluation_wildcard))),
            ].iter().map(|(k, v)| {
                (k.to_string(), v.clone())
            }).collect();

            let input = handle_input(input_fields, true);

            let res = projects::patch_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
                input.get("New Name").cloned(),
                input.get("Display Name").cloned(),
                input.get("Description").cloned(),
                input.get("Repository").cloned(),
                input.get("Evaluation Wildcard").cloned(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Project creation failed: {}", res.message);
                exit(1);
            }

            println!("Project edited.");
        }

        Commands::Delete => {
            let (organization, project) =
                match set_get_value(ConfigKey::SelectedProject, None, true) {
                    Some(id) => {
                        let parts: Vec<&str> = id.split("/").collect();
                        (parts[0].to_string(), parts[1].to_string())
                    }
                    None => {
                        eprintln!("Project is required for command.");
                        exit(1);
                    }
                };

            let res = projects::delete_project(
                get_request_config(load_config()).unwrap(),
                organization,
                project,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Failed to delete project.");
                exit(1);
            }

            println!("Project deleted.");
        }
    }
}
