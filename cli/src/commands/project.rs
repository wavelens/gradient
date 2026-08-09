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
use connector::projects::{
    AddUserRequest, MakeProjectRequest, PatchProjectRequest, RemoveUserRequest,
};
use connector::{Client, ConnectorError};

#[derive(Subcommand, Debug)]
pub enum Commands {
    Select {
        #[arg(add = ArgValueCompleter::new(completion::complete_projects))]
        project: String,
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
    Add {
        user: String,
        role: Option<String>,
    },
    Remove {
        #[arg(add = ArgValueCompleter::new(completion::complete_project_users))]
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
        Commands::Select { project } => {
            let memberships = membership_names(out).await;
            if !memberships.iter().any(|o| o == &project) {
                out.err(
                    ExitKind::Usage,
                    format!(
                        "You are not a member of project '{}'. Your projects: {}",
                        project,
                        if memberships.is_empty() {
                            "(none)".to_string()
                        } else {
                            memberships.join(", ")
                        }
                    ),
                );
            }
            set_get_value(ConfigKey::SelectedProject, Some(project), true).unwrap();
            out.human("Project selected.");
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
                .projects()
                .create(MakeProjectRequest {
                    name: name.clone(),
                    display_name: input.get("Display Name").unwrap().clone(),
                    description: input.get("Description").unwrap().clone(),
                })
                .await
            {
                Ok(_) => {
                    set_get_value(ConfigKey::SelectedProject, Some(name), true);
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Project created.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Show => {
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);
            match client.projects().get(&project).await {
                Ok(project) => {
                    out.ok(&project);
                    out.human(format!("Name: {}", project.name));
                    out.human(format!("Description: {}", project.description));
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::List => {
            let client = client_from_config(out);
            match client.projects().list().await {
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
        } => {
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);
            let current = match client.projects().get(&project).await {
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
                .projects()
                .update(
                    &project,
                    PatchProjectRequest {
                        name: input.get("Name").cloned(),
                        display_name: input.get("Display Name").cloned(),
                        description: input.get("Description").cloned(),
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
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);
            match client.projects().delete(&project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Project deleted.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::User { cmd } => {
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                UserCommands::List => match client.projects().users(&project).await {
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
                        .projects()
                        .add_user(
                            &project,
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
                        .projects()
                        .remove_user(&project, RemoveUserRequest { user })
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
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                SshCommands::Show => match client.projects().ssh_key(&project).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("Public Key: {}", key));
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },

                SshCommands::Recreate => match client.projects().regenerate_ssh(&project).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("New Public Key: {}", key));
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                },
            }
        }

        Commands::Cache { cmd } => {
            let project = match set_get_value(ConfigKey::SelectedProject, None, true) {
                Some(id) => id,
                None => out.err(ExitKind::Usage, "Project is required for command."),
            };

            let client = client_from_config(out);

            match cmd {
                CacheCommands::List => match client.projects().subscriptions(&project).await {
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
                    match client.projects().subscribe(&project, &cache).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"subscribed": true}));
                            out.human("Subscribed to cache.");
                        }
                        Err(e) => out.err(to_exit_kind(&e), e),
                    }
                }

                CacheCommands::Remove { cache } => {
                    match client.projects().unsubscribe(&project, &cache).await {
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

/// Names of the projects the current user belongs to, exiting with a clear
/// login hint when no session is configured or the server rejects it.
async fn membership_names(out: Output) -> Vec<String> {
    if set_get_value(ConfigKey::AuthToken, None, true).is_none() {
        out.err(
            ExitKind::Unauthorized,
            "Not logged in. Run `gradient login <url>` first.",
        );
    }
    let client = client_from_config(out);
    match client.projects().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(ConnectorError::Unauthorized) => out.err(
            ExitKind::Unauthorized,
            "Not logged in. Run `gradient login <url>` first.",
        ),
        Err(e) => out.err(to_exit_kind(&e), e),
    }
}

/// After a successful login, select the user's project when it is
/// unambiguous, otherwise guide them. Never blocks login on a list failure.
pub async fn post_login_project_setup(client: &Client, out: Output) {
    let projects: Vec<String> = match client.projects().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(_) => return,
    };
    let current = set_get_value(ConfigKey::SelectedProject, None, true);
    match decide_project_onboarding(&projects, current.as_deref()) {
        ProjectOnboarding::Keep(_) => {}
        ProjectOnboarding::AutoSelect(name) => {
            set_get_value(ConfigKey::SelectedProject, Some(name.clone()), true);
            out.human(format!("Selected project {name}."));
        }
        ProjectOnboarding::Choose(names) => {
            out.human("You belong to multiple projects:");
            for n in &names {
                out.human(format!("  {n}"));
            }
            out.human("Select one with `gradient project select <name>`.");
        }
        ProjectOnboarding::None => out.human(
            "You are not a member of any project yet. Create one with `gradient project create`.",
        ),
    }
}

/// Post-login project handling derived from the user's memberships and any current
/// selection. Pure so the decision is testable without a server.
#[derive(Debug, PartialEq, Eq)]
pub enum ProjectOnboarding {
    Keep(String),
    AutoSelect(String),
    Choose(Vec<String>),
    None,
}

pub fn decide_project_onboarding(projects: &[String], current: Option<&str>) -> ProjectOnboarding {
    if let Some(sel) = current
        && projects.iter().any(|o| o == sel)
    {
        return ProjectOnboarding::Keep(sel.to_string());
    }
    match projects {
        [] => ProjectOnboarding::None,
        [one] => ProjectOnboarding::AutoSelect(one.clone()),
        _ => ProjectOnboarding::Choose(projects.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectOnboarding, decide_project_onboarding};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_projects_yields_none() {
        assert_eq!(
            decide_project_onboarding(&[], None),
            ProjectOnboarding::None
        );
    }

    #[test]
    fn single_project_auto_selects() {
        assert_eq!(
            decide_project_onboarding(&v(&["solo"]), None),
            ProjectOnboarding::AutoSelect("solo".into())
        );
    }

    #[test]
    fn multiple_projects_prompt_choice() {
        assert_eq!(
            decide_project_onboarding(&v(&["a", "b"]), None),
            ProjectOnboarding::Choose(v(&["a", "b"]))
        );
    }

    #[test]
    fn valid_current_selection_is_kept() {
        assert_eq!(
            decide_project_onboarding(&v(&["a", "b"]), Some("b")),
            ProjectOnboarding::Keep("b".into())
        );
    }

    #[test]
    fn stale_current_selection_falls_through() {
        assert_eq!(
            decide_project_onboarding(&v(&["a"]), Some("c")),
            ProjectOnboarding::AutoSelect("a".into())
        );
    }
}
