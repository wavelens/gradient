/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
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
    Select {
        organization: String,
    },
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
    },
    Show,
    List,
    Edit {
        #[arg(short, long)]
        new_name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
    },
    Delete,
    User {
        #[command(subcommand)]
        cmd: UserCommands,
    },
    Ssh {
        #[command(subcommand)]
        cmd: SshCommands,
    },
    Cache {
        #[command(subcommand)]
        cmd: CacheCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum UserCommands {
    List,
    Add { user: String, role: Option<String> },
    Remove { user: String },
}

#[derive(Subcommand, Debug)]
pub enum SshCommands {
    Show,
    Recreate,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommands {
    List,
    Add { cache: String },
    Remove { cache: String },
}

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Select { organization } => {
            set_get_value(ConfigKey::SelectedOrganization, Some(organization), true).unwrap();
            println!("Organization selected.");
        }

        Commands::Create {
            name,
            display_name,
            description,
        } => {
            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let res = orgs::put(
                get_request_config(load_config()).unwrap(),
                name.clone(),
                input.get("Display Name").unwrap().clone(),
                input.get("Description").unwrap().clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Organization creation failed: {}", res.message);
                exit(1);
            }

            set_get_value(ConfigKey::SelectedOrganization, Some(name), true);
            println!("Organization created.");
        }

        Commands::Show => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let res =
                orgs::get_organization(get_request_config(load_config()).unwrap(), organization)
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

            if res.error {
                eprintln!("Failed to show organization.");
                exit(1);
            }

            println!("Name: {}", res.message.name);
            println!("Description: {}", res.message.description);
            println!("Use Nix Store: {}", res.message.use_nix_store);
        }

        Commands::List => {
            let res = orgs::get(get_request_config(load_config()).unwrap())
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

            if res.error {
                eprintln!("Failed to list organizations");
                exit(1);
            }

            if res.message.is_empty() {
                println!("You have no organizations.");
            } else {
                for org in res.message {
                    println!("{}: {}", org.name, org.id);
                }
            }
        }

        Commands::Edit {
            new_name,
            display_name,
            description,
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let current_organization = orgs::get_organization(
                get_request_config(load_config()).unwrap(),
                organization.clone(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap()
            .message;

            let input_fields = [
                ("Name", Some(new_name.unwrap_or(current_organization.name))),
                (
                    "Display Name",
                    Some(display_name.unwrap_or(current_organization.display_name)),
                ),
                (
                    "Description",
                    Some(description.unwrap_or(current_organization.description)),
                ),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);

            let res = orgs::patch_organization(
                get_request_config(load_config()).unwrap(),
                organization,
                input.get("Name").cloned(),
                input.get("Display Name").cloned(),
                input.get("Description").cloned(),
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Organization update failed: {}", res.message);
                exit(1);
            }

            println!("Organization updated.");
        }

        Commands::Delete => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            let res =
                orgs::delete_organization(get_request_config(load_config()).unwrap(), organization)
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

            if res.error {
                eprintln!("Failed to delete organization: {}", res.message);
                exit(1);
            }

            println!("Organization deleted.");
        }

        Commands::User { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            match cmd {
                UserCommands::List => {
                    let res = orgs::get_organization_users(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to list users");
                        exit(1);
                    }

                    if res.message.is_empty() {
                        println!("You have no users.");
                    } else {
                        for user in res.message {
                            println!("{}: {}", user.name, user.id);
                        }
                    }
                }

                UserCommands::Add { user, role } => {
                    if role.is_some()
                        && role.as_ref().unwrap() != "View"
                        && role.as_ref().unwrap() != "Write"
                        && role.as_ref().unwrap() != "Admin"
                    {
                        eprintln!("Role must be either 'View', 'Write' or 'Admin'.");
                        exit(1);
                    }

                    let res = orgs::post_organization_users(
                        get_request_config(load_config()).unwrap(),
                        organization,
                        user,
                        role.unwrap_or("Write".to_string()),
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to add user: {}", res.message);
                        exit(1);
                    }

                    println!("User added.");
                }

                UserCommands::Remove { user } => {
                    let res = orgs::delete_organization_users(
                        get_request_config(load_config()).unwrap(),
                        organization,
                        user,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to remove user: {}", res.message);
                        exit(1);
                    }

                    println!("User removed.");
                }
            }
        }

        Commands::Ssh { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            match cmd {
                SshCommands::Show => {
                    let res = orgs::get_organization_ssh(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to show SSH key: {}", res.message);
                        exit(1);
                    }

                    println!("Public Key: {}", res.message);
                }

                SshCommands::Recreate => {
                    let res = orgs::post_organization_ssh(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to recreate SSH key: {}", res.message);
                        exit(1);
                    }

                    println!("New Public Key: {}", res.message);
                }
            }
        }

        Commands::Cache { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => {
                    eprintln!("Organization is required for command.");
                    exit(1);
                }
            };

            match cmd {
                CacheCommands::List => {
                    let res = orgs::get_organization_subscribe(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("failed to list subscribed caches");
                        exit(1);
                    }

                    if res.message.is_empty() {
                        println!("You have no caches subscribed.");
                    } else {
                        for cache in res.message {
                            println!("{}: {}", cache.name, cache.id);
                        }
                    }
                }

                CacheCommands::Add { cache } => {
                    let res = orgs::post_organization_subscribe_cache(
                        get_request_config(load_config()).unwrap(),
                        organization,
                        cache,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to subscribe to cache: {}", res.message);
                        exit(1);
                    }

                    println!("Subscribed to cache.");
                }

                CacheCommands::Remove { cache } => {
                    let res = orgs::delete_organization_subscribe_cache(
                        get_request_config(load_config()).unwrap(),
                        organization,
                        cache,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to unsubscribe from cache: {}", res.message);
                        exit(1);
                    }

                    println!("Unsubscribed from cache.");
                }
            }
        }
    }
}
