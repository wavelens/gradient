/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use clap::{Subcommand, arg};
use connector::*;
use std::process::exit;

#[derive(Subcommand, Debug)]
pub enum Commands {
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 's', long)]
        host: Option<String>,
        #[arg(short, long)]
        port: Option<i32>,
        #[arg(short = 'u', long)]
        ssh_user: Option<String>,
        #[arg(short, long)]
        architectures: Option<String>,
        #[arg(short, long)]
        features: Option<String>,
    },
    List,
    Edit {
        name: String,
        #[arg(short, long)]
        new_name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 's', long)]
        host: Option<String>,
        #[arg(short, long)]
        port: Option<i32>,
        #[arg(short = 'u', long)]
        ssh_user: Option<String>,
    },
    Delete {
        name: String,
    },
}

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Create {
            name,
            display_name,
            host,
            port,
            ssh_user,
            architectures,
            features,
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Host", host),
                ("Port", port.map(|p| p.to_string())),
                ("SSH User", ssh_user),
                ("Architectures", architectures),
                ("Features", features),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let res = servers::put(
                get_request_config(load_config()).unwrap(),
                organization,
                name,
                input.get("Display Name").unwrap().clone(),
                input.get("Host").unwrap().clone(),
                input.get("Port").unwrap().parse().unwrap(),
                input.get("SSH User").unwrap().clone(),
                input
                    .get("Architectures")
                    .unwrap()
                    .split(",")
                    .map(|s| s.to_string())
                    .collect(),
                input
                    .get("Features")
                    .unwrap()
                    .split(",")
                    .map(|s| s.to_string())
                    .collect(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Server creation failed: {}", res.message);
                exit(1);
            }

            println!("Server created.");
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let res = servers::get(get_request_config(load_config()).unwrap(), organization)
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

            if res.error {
                eprintln!("Failed to list servers");
                exit(1);
            }

            if res.message.is_empty() {
                println!("You have no servers.");
            } else {
                for server in res.message {
                    println!("{}: {}", server.name, server.id);
                }
            }
        }

        Commands::Edit {
            name,
            new_name,
            display_name,
            host,
            port,
            ssh_user,
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let current_server = servers::get_server(
                get_request_config(load_config()).unwrap(),
                organization.clone(),
                name.clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap()
            .message;

            let input_fields = [
                ("Name", Some(new_name.unwrap_or(current_server.name))),
                (
                    "Display Name",
                    Some(display_name.unwrap_or(current_server.display_name)),
                ),
                ("Host", Some(host.unwrap_or(current_server.host))),
                (
                    "Port",
                    port.map(|p| p.to_string())
                        .or(Some(current_server.port.to_string())),
                ),
                ("SSH User", ssh_user.or(Some(current_server.username))),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false);

            let res = servers::patch_server(
                get_request_config(load_config()).unwrap(),
                organization,
                name.clone(),
                input.get("Name").cloned(),
                input.get("Display Name").cloned(),
                input.get("Host").cloned(),
                input.get("Port").map(|p| p.parse().unwrap()),
                input.get("SSH User").cloned(),
                None,
                None,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Server creation failed: {}", res.message);
                exit(1);
            }

            println!("Server updated.");
        }

        Commands::Delete { name } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let res = servers::delete_server(
                get_request_config(load_config()).unwrap(),
                organization,
                name.clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Server deletion failed: {}", res.message);
                exit(1);
            }

            println!("Server deleted.");
        }
    }
}
