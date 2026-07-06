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
use connector::{Client, ConnectorError};

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
            let memberships = membership_names(out).await;
            if !memberships.iter().any(|o| o == &organization) {
                out.err(
                    ExitKind::Usage,
                    format!(
                        "You are not a member of organization '{}'. Your organizations: {}",
                        organization,
                        if memberships.is_empty() {
                            "(none)".to_string()
                        } else {
                            memberships.join(", ")
                        }
                    ),
                );
            }
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

/// Names of the organizations the current user belongs to, exiting with a clear
/// login hint when no session is configured or the server rejects it.
async fn membership_names(out: Output) -> Vec<String> {
    if set_get_value(ConfigKey::AuthToken, None, true).is_none() {
        out.err(
            ExitKind::Unauthorized,
            "Not logged in. Run `gradient login <url>` first.",
        );
    }
    let client = client_from_config(out);
    match client.orgs().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(ConnectorError::Unauthorized) => out.err(
            ExitKind::Unauthorized,
            "Not logged in. Run `gradient login <url>` first.",
        ),
        Err(e) => out.err(to_exit_kind(&e), e),
    }
}

/// After a successful login, select the user's organization when it is
/// unambiguous, otherwise guide them. Never blocks login on a list failure.
pub async fn post_login_org_setup(client: &Client, out: Output) {
    let orgs: Vec<String> = match client.orgs().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(_) => return,
    };
    let current = set_get_value(ConfigKey::SelectedOrganization, None, true);
    match decide_org_onboarding(&orgs, current.as_deref()) {
        OrgOnboarding::Keep(_) => {}
        OrgOnboarding::AutoSelect(name) => {
            set_get_value(ConfigKey::SelectedOrganization, Some(name.clone()), true);
            out.human(format!("Selected organization {name}."));
        }
        OrgOnboarding::Choose(names) => {
            out.human("You belong to multiple organizations:");
            for n in &names {
                out.human(format!("  {n}"));
            }
            out.human("Select one with `gradient organization select <name>`.");
        }
        OrgOnboarding::None => out.human(
            "You are not a member of any organization yet. Create one with `gradient organization create`.",
        ),
    }
}

/// Post-login org handling derived from the user's memberships and any current
/// selection. Pure so the decision is testable without a server.
#[derive(Debug, PartialEq, Eq)]
pub enum OrgOnboarding {
    Keep(String),
    AutoSelect(String),
    Choose(Vec<String>),
    None,
}

pub fn decide_org_onboarding(orgs: &[String], current: Option<&str>) -> OrgOnboarding {
    if let Some(sel) = current
        && orgs.iter().any(|o| o == sel)
    {
        return OrgOnboarding::Keep(sel.to_string());
    }
    match orgs {
        [] => OrgOnboarding::None,
        [one] => OrgOnboarding::AutoSelect(one.clone()),
        _ => OrgOnboarding::Choose(orgs.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::{OrgOnboarding, decide_org_onboarding};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_orgs_yields_none() {
        assert_eq!(decide_org_onboarding(&[], None), OrgOnboarding::None);
    }

    #[test]
    fn single_org_auto_selects() {
        assert_eq!(
            decide_org_onboarding(&v(&["solo"]), None),
            OrgOnboarding::AutoSelect("solo".into())
        );
    }

    #[test]
    fn multiple_orgs_prompt_choice() {
        assert_eq!(
            decide_org_onboarding(&v(&["a", "b"]), None),
            OrgOnboarding::Choose(v(&["a", "b"]))
        );
    }

    #[test]
    fn valid_current_selection_is_kept() {
        assert_eq!(
            decide_org_onboarding(&v(&["a", "b"]), Some("b")),
            OrgOnboarding::Keep("b".into())
        );
    }

    #[test]
    fn stale_current_selection_falls_through() {
        assert_eq!(
            decide_org_onboarding(&v(&["a"]), Some("c")),
            OrgOnboarding::AutoSelect("a".into())
        );
    }
}
