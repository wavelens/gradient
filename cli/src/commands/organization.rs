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
use connector::orgs::{
    AddUserRequest, MakeOrganizationRequest, PatchOrganizationRequest, RemoveUserRequest,
};

#[derive(Subcommand, Debug)]
pub enum Commands {
    Select {
        #[arg(add = ArgValueCompleter::new(completion::complete_orgs))]
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
    Remove {
        #[arg(add = ArgValueCompleter::new(completion::complete_org_users))]
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SshCommands {
    Show,
    Recreate,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommands {
    List,
    Add {
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    Remove {
        #[arg(add = ArgValueCompleter::new(completion::complete_subscribed_caches))]
        cache: String,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Select { organization } => {
            set_get_value(ConfigKey::SelectedOrganization, Some(organization), true).unwrap();
            out.human("Organization selected.");
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

            let client = client_from_config(out);
            match client
                .orgs()
                .create(MakeOrganizationRequest {
                    name: name.clone(),
                    display_name: input.get("Display Name").unwrap().clone(),
                    description: input.get("Description").unwrap().clone(),
                })
                .await
            {
                Ok(_) => {
                    set_get_value(ConfigKey::SelectedOrganization, Some(name), true);
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Organization created.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Show => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);
            match client.orgs().get(&organization).await {
                Ok(org) => {
                    out.ok(&org);
                    out.human(format!("Name: {}", org.name));
                    out.human(format!("Description: {}", org.description));
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::List => {
            let client = client_from_config(out);
            match client.orgs().list().await {
                Ok(res) => {
                    out.ok(&res);
                    if res.items.is_empty() {
                        out.human("You have no organizations.");
                    } else {
                        for org in res.items {
                            out.human(format!("{}: {}", org.name, org.id));
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
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);
            let current = match client.orgs().get(&organization).await {
                Ok(o) => o,
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
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);

            match client
                .orgs()
                .update(
                    &organization,
                    PatchOrganizationRequest {
                        name: input.get("Name").cloned(),
                        display_name: input.get("Display Name").cloned(),
                        description: input.get("Description").cloned(),
                    },
                )
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Organization updated.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Delete => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);
            match client.orgs().delete(&organization).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Organization deleted.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::User { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                UserCommands::List => match client.orgs().users(&organization).await {
                    Ok(users) => {
                        out.ok(&users);
                        if users.is_empty() {
                            out.human("You have no users.");
                        } else {
                            for user in users {
                                out.human(format!("{}: {}", user.name, user.id));
                            }
                        }
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },

                UserCommands::Add { user, role } => {
                    if role
                        .as_deref()
                        .map(|r| r != "View" && r != "Write" && r != "Admin")
                        .unwrap_or(false)
                    {
                        out.err(
                            ExitKind::Usage,
                            "Role must be either 'View', 'Write' or 'Admin'.",
                        );
                    }

                    match client
                        .orgs()
                        .add_user(
                            &organization,
                            AddUserRequest {
                                user,
                                role: role.unwrap_or_else(|| "Write".to_string()),
                            },
                        )
                        .await
                    {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"added": true}));
                            out.human("User added.");
                        }
                        Err(e) => out.err(to_exit_kind(&e), e),
                    }
                }

                UserCommands::Remove { user } => {
                    match client
                        .orgs()
                        .remove_user(&organization, RemoveUserRequest { user })
                        .await
                    {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"removed": true}));
                            out.human("User removed.");
                        }
                        Err(e) => out.err(to_exit_kind(&e), e),
                    }
                }
            }
        }

        Commands::Ssh { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                SshCommands::Show => match client.orgs().ssh_key(&organization).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("Public Key: {}", key));
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },

                SshCommands::Recreate => match client.orgs().regenerate_ssh(&organization).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("New Public Key: {}", key));
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },
            }
        }

        Commands::Cache { cmd } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Organization is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                CacheCommands::List => match client.orgs().subscriptions(&organization).await {
                    Ok(caches) => {
                        out.ok(&caches);
                        if caches.is_empty() {
                            out.human("You have no caches subscribed.");
                        } else {
                            for cache in caches {
                                out.human(format!("{}: {}", cache.name, cache.id));
                            }
                        }
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },

                CacheCommands::Add { cache } => {
                    match client.orgs().subscribe(&organization, &cache).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"subscribed": true}));
                            out.human("Subscribed to cache.");
                        }
                        Err(e) => out.err(to_exit_kind(&e), e),
                    }
                }

                CacheCommands::Remove { cache } => {
                    match client.orgs().unsubscribe(&organization, &cache).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"unsubscribed": true}));
                            out.human("Unsubscribed from cache.");
                        }
                        Err(e) => out.err(to_exit_kind(&e), e),
                    }
                }
            }
        }
    }
}
