/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::commands::webhook::{
    NoWebhookOwnerArguments, WebhookSubcommands, run_webhook_subcommand,
};
use crate::config::*;
use crate::error::CliError;
use crate::input::{
    ask_for_choice, ask_for_secret, client_from_config, handle_input, is_interactive,
    selected_project,
};
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::{
    AddMemberRequest, GrantProjectRequest, MakeProjectRequest, PatchProjectGrantRequest,
    PatchProjectRequest, PutProjectSecretRequest, RemoveMemberRequest, RevokeInvitationRequest,
};
use connector::webhooks::WebhookResource;
use connector::{Client, ConnectorError};
use std::io::{self, IsTerminal, Read};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Select the project later commands act on
    Select {
        /// Project name
        #[arg(add = ArgValueCompleter::new(completion::complete_projects))]
        project: String,
    },
    /// Create a project with missing fields opened in $EDITOR, then select it unless `--json` is set
    Create {
        /// Name
        #[arg(short, long)]
        name: Option<String>,
        /// Display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// Description
        #[arg(short = 'c', long)]
        description: Option<String>,
    },
    /// Show the selected project
    Show,
    /// List the projects of the current user
    List,
    /// Edit the selected project
    Edit {
        /// New name
        #[arg(short, long)]
        new_name: Option<String>,
        /// New display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// New description
        #[arg(short = 'c', long)]
        description: Option<String>,
    },
    /// Delete the selected project
    Delete,
    /// Manage members of the selected project
    User {
        #[command(subcommand)]
        cmd: UserCommands,
    },
    /// Manage the SSH key of the selected project
    Ssh {
        #[command(subcommand)]
        cmd: SshCommands,
    },
    /// Manage the caches the selected project is pushing to
    Cache {
        #[command(subcommand)]
        cmd: CacheCommands,
    },
    /// Manage the secrets that exec jobs can request for the selected project. Values are write-only.
    Secret {
        #[command(subcommand)]
        cmd: ProjectSecretCommands,
    },
    /// Print the SOPS recipient to encrypt the sops files in the repository
    ShowSopsRecipient,
    /// Allow visitors to view selected projects without login.
    MakePublic,
    /// Restrict the selected project to its members.
    MakeProjectPrivate,
    /// Manage teams that can access the selected project
    TeamGrant {
        #[command(subcommand)]
        cmd: ProjectTeamGrantCommands,
    },
    /// Manage pending membership invitations for the selected project.
    PendingMembershipInvitation {
        #[command(subcommand)]
        cmd: ProjectPendingInvitationCommands,
    },
    /// Manage webhooks for the selected project.
    Webhook {
        #[command(subcommand)]
        cmd: WebhookSubcommands<NoWebhookOwnerArguments>,
    },
}

#[derive(Subcommand, Debug)]
pub enum UserCommands {
    /// List members
    List,
    /// Add a member
    Add {
        /// Username
        user: String,
        /// Role: View, Write or Admin (default: Write)
        role: Option<String>,
    },
    /// Remove a member
    Remove {
        /// Username
        #[arg(add = ArgValueCompleter::new(completion::complete_project_users))]
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SshCommands {
    /// Print the public SSH key used to fetch repositories
    Show,
    /// Replace the SSH key pair and print the new public key
    Recreate,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommands {
    /// List subscribed caches
    List,
    /// Subscribe to a cache
    Add {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Unsubscribe from a cache
    Remove {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_subscribed_caches))]
        cache: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectSecretCommands {
    /// List secret names
    List,
    /// Display the timestamp of the last secret update and the person who made it. Never display the value.
    Show {
        /// Secret name
        name: String,
    },
    /// Create or replace a secret. The command can read the value from piped stdin or a hidden prompt with no `--value` argument.
    Set {
        /// Secret name
        name: String,
        /// Secret value. Flag values can remain in shell history or the process list. Use stdin instead.
        #[arg(long)]
        value: Option<String>,
    },
    /// Delete a secret
    Delete {
        /// Secret name
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectTeamGrantCommands {
    /// List grants for a team (including pending requests)
    List,
    /// Grant team access. A non-admin user can send a request to the team admin.
    Add {
        /// Team name
        team: String,
        /// Role of team users in a project: `View`, `Write` or `Admin`
        #[arg(long)]
        role: Option<String>,
        /// Allow access for team users
        #[arg(long)]
        users: bool,
        /// Build the project on the team workers.
        #[arg(long)]
        workers: bool,
    },
    /// Edit the role or access of a team grant.
    Edit {
        /// Team name
        team: String,
        /// Role of team users in a project: `View`, `Write` or `Admin`
        #[arg(long)]
        role: Option<String>,
        /// Grant (true) or revoke (false) access rights for team users.
        #[arg(long)]
        users: Option<bool>,
        /// Allow team workers to run builds for the project (`true`) or stop the builds (`false`).
        #[arg(long)]
        workers: Option<bool>,
    },
    /// Delete the team grant or withdraw the pending request.
    Remove {
        /// Team name
        team: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectPendingInvitationCommands {
    /// List pending invitations
    List,
    /// Cancel a user's pending invitations
    Revoke {
        /// Username
        user: String,
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
            set_value(ConfigKey::SelectedProject, project.clone(), true);
            out.ok(&serde_json::json!({"selected": true, "project": project}));
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

            let input = handle_input(input_fields, true, out);
            let name = input.get("Name");

            let client = client_from_config(out);
            match client
                .projects()
                .create(MakeProjectRequest {
                    name: name.clone().into(),
                    display_name: input.get("Display Name").into(),
                    description: input.get("Description"),
                    hide_build_requests: None,
                    public: None,
                })
                .await
            {
                Ok(_) => {
                    if !out.is_json() {
                        set_value(ConfigKey::SelectedProject, name, true);
                    }
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Project created.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Show => {
            let project = selected_project(out);

            let client = client_from_config(out);
            match client.projects().get(&project).await {
                Ok(project) => {
                    out.ok(&project);
                    out.human(format!("Name: {}", project.name));
                    out.human(format!("Description: {}", project.description));
                }
                Err(e) => out.fail(e),
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
                Err(e) => out.fail(e),
            }
        }

        Commands::Edit {
            new_name,
            display_name,
            description,
        } => {
            let project = selected_project(out);

            let client = client_from_config(out);
            let current = match client.projects().get(&project).await {
                Ok(o) => o,
                Err(e) => out.fail(e),
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

            let input = handle_input(input_fields, true, out);

            match client
                .projects()
                .update(
                    &project,
                    PatchProjectRequest {
                        name: (input.get("Name") != project).then(|| input.get("Name").into()),
                        display_name: Some(input.get("Display Name")),
                        description: Some(input.get("Description")),
                        hide_build_requests: None,
                        allow_exec_in_build_requests: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    let renamed = input.get("Name");
                    if renamed != project {
                        update_or_create_saved_login_in_config_file(|login| {
                            if login.selected_project.as_deref() == Some(project.as_str()) {
                                login.selected_project = Some(renamed.clone());
                            }
                            let prefix = format!("{project}/");
                            if let Some(task) = login
                                .selected_task
                                .as_deref()
                                .and_then(|selected| selected.strip_prefix(&prefix))
                            {
                                login.selected_task = Some(format!("{renamed}/{task}"));
                            }
                        });
                    }
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Project updated.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete => {
            let project = selected_project(out);

            let client = client_from_config(out);
            match client.projects().delete(&project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Project deleted.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::User { cmd } => {
            let project = selected_project(out);

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
                    Err(e) => out.fail(e),
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
                            AddMemberRequest {
                                user,
                                role: role.unwrap_or_else(|| "Write".to_string()),
                            },
                        )
                        .await
                    {
                        Ok(message) => {
                            out.ok(&serde_json::json!({"added": true, "message": message}));
                            out.human(format!("{message}."));
                        }
                        Err(e) => out.fail(e),
                    }
                }

                UserCommands::Remove { user } => {
                    match client
                        .projects()
                        .remove_user(&project, RemoveMemberRequest { user })
                        .await
                    {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"removed": true}));
                            out.human("User removed.");
                        }
                        Err(e) => out.fail(e),
                    }
                }
            }
        }

        Commands::Ssh { cmd } => {
            let project = selected_project(out);

            let client = client_from_config(out);

            match cmd {
                SshCommands::Show => match client.projects().ssh_key(&project).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("Public Key: {}", key));
                    }
                    Err(e) => out.fail(e),
                },

                SshCommands::Recreate => match client.projects().regenerate_ssh(&project).await {
                    Ok(key) => {
                        out.ok(&serde_json::json!({"public_key": key}));
                        out.human(format!("New Public Key: {}", key));
                    }
                    Err(e) => out.fail(e),
                },
            }
        }

        Commands::Cache { cmd } => {
            let project = selected_project(out);

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
                    Err(e) => out.fail(e),
                },

                CacheCommands::Add { cache } => {
                    match client.projects().subscribe(&project, &cache).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"subscribed": true}));
                            out.human("Subscribed to cache.");
                        }
                        Err(e) => out.fail(e),
                    }
                }

                CacheCommands::Remove { cache } => {
                    match client.projects().unsubscribe(&project, &cache).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"unsubscribed": true}));
                            out.human("Unsubscribed from cache.");
                        }
                        Err(e) => out.fail(e),
                    }
                }
            }
        }

        Commands::Webhook { cmd } => {
            run_webhook_subcommand(
                cmd,
                |_| WebhookResource::Project(selected_project(out)),
                out,
            )
            .await
        }

        Commands::Secret { cmd } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            let projects = client.projects();

            match cmd {
                ProjectSecretCommands::List => {
                    match projects.list_secret_summaries(&project).await {
                        Ok(secrets) => {
                            out.ok(&secrets);
                            if secrets.is_empty() {
                                out.human("No secrets in the project.");
                            }
                            for secret in secrets {
                                out.human(format!("{}: {}", secret.name, secret.updated_at));
                            }
                        }
                        Err(e) => out.fail(e),
                    }
                }

                ProjectSecretCommands::Show { name } => {
                    match projects.get_project_secret_summary(&project, &name).await {
                        Ok(secret) => {
                            out.ok(&secret);
                            out.human(format!("Name: {}", secret.name));
                            out.human(format!("Updated at: {}", secret.updated_at));
                            if let Some(by) = &secret.updated_by {
                                out.human(format!("Updated by: {by}"));
                            }
                            out.human(format!("Managed: {}", secret.managed));
                        }
                        Err(e) => out.fail(e),
                    }
                }

                ProjectSecretCommands::Set { name, value } => {
                    let value = read_project_secret_value_from_flag_stdin_or_hidden_prompt(
                        &name, value, out,
                    );
                    match projects
                        .put_project_secret(&project, &name, PutProjectSecretRequest { value })
                        .await
                    {
                        Ok(secret) => {
                            out.ok(&secret);
                            out.human("Secret saved.");
                        }
                        Err(e) => out.fail(e),
                    }
                }

                ProjectSecretCommands::Delete { name } => {
                    match projects.delete_project_secret(&project, &name).await {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"deleted": true, "name": name}));
                            out.human("Deleted the secret.");
                        }
                        Err(e) => out.fail(e),
                    }
                }
            }
        }

        Commands::ShowSopsRecipient => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client.projects().get_project_sops_recipient(&project).await {
                Ok(recipient) => {
                    out.ok(&recipient);
                    if let Some(age) = &recipient.age {
                        out.human(format!("age: {age}"));
                    }
                    out.human(format!("ssh: {}", recipient.ssh));
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::MakePublic => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client.projects().make_project_public(&project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"public": true, "project": project}));
                    out.human("The project is now public.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::MakeProjectPrivate => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client.projects().make_project_private(&project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"public": false, "project": project}));
                    out.human("The project is now private.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::TeamGrant { cmd } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            let projects = client.projects();

            let result = match cmd {
                ProjectTeamGrantCommands::List => {
                    match projects.list_project_team_grants(&project).await {
                        Ok(grants) => {
                            out.ok(&grants);
                            if grants.is_empty() {
                                out.human("No teams have access to the project.");
                            }
                            for grant in grants {
                                out.human(format!(
                                    "{}: role={} users={} workers={} pending={}",
                                    grant.team,
                                    grant.role.as_deref().unwrap_or("-"),
                                    grant.users,
                                    grant.workers,
                                    grant.pending
                                ));
                            }
                        }
                        Err(e) => out.fail(e),
                    }
                    return;
                }
                ProjectTeamGrantCommands::Add {
                    team,
                    role,
                    users,
                    workers,
                } => {
                    projects
                        .grant_team_access_to_project(
                            &project,
                            GrantProjectRequest {
                                team,
                                role,
                                users: Some(users),
                                workers: Some(workers),
                            },
                        )
                        .await
                }
                ProjectTeamGrantCommands::Edit {
                    team,
                    role,
                    users,
                    workers,
                } => {
                    projects
                        .update_project_team_grant(
                            &project,
                            &team,
                            PatchProjectGrantRequest {
                                role,
                                users,
                                workers,
                            },
                        )
                        .await
                }
                ProjectTeamGrantCommands::Remove { team } => {
                    projects.remove_project_team_grant(&project, &team).await
                }
            };
            match result {
                Ok(message) => {
                    out.ok(&serde_json::json!({"message": message}));
                    out.human(format!("{message}."));
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::PendingMembershipInvitation { cmd } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            let projects = client.projects();

            match cmd {
                ProjectPendingInvitationCommands::List => {
                    match projects.list_pending_project_invitations(&project).await {
                        Ok(invitations) => {
                            out.ok(&invitations);
                            if invitations.is_empty() {
                                out.human("No pending invitations in the project.");
                            }
                            for invitation in invitations {
                                out.human(format!(
                                    "{}: {} until {}",
                                    invitation.user, invitation.role, invitation.expires_at
                                ));
                            }
                        }
                        Err(e) => out.fail(e),
                    }
                }
                ProjectPendingInvitationCommands::Revoke { user } => {
                    match projects
                        .revoke_project_invitation(&project, RevokeInvitationRequest { user })
                        .await
                    {
                        Ok(_) => {
                            out.ok(&serde_json::json!({"revoked": true}));
                            out.human("Invitation revoked.");
                        }
                        Err(e) => out.fail(e),
                    }
                }
            }
        }
    }
}

fn read_project_secret_value_from_flag_stdin_or_hidden_prompt(
    name: &str,
    value: Option<String>,
    out: Output,
) -> String {
    if let Some(value) = value {
        return value;
    }
    if io::stdin().is_terminal() {
        if out.is_json() {
            out.fail(CliError::MissingArgument("--value"));
        }
        return ask_for_secret(name, out);
    }
    let mut piped = String::new();
    io::stdin()
        .read_to_string(&mut piped)
        .unwrap_or_else(|e| out.fail(CliError::ReadInput(name.to_owned(), e)));
    let value = piped
        .strip_suffix('\n')
        .map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
        .unwrap_or(&piped);
    if value.is_empty() {
        out.fail(CliError::EmptyField(name.to_owned()));
    }
    value.to_owned()
}

async fn membership_names(out: Output) -> Vec<String> {
    if get_value(ConfigKey::AuthToken, true).is_none() {
        out.fail(CliError::NotLoggedIn);
    }
    let client = client_from_config(out);
    match client.projects().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(ConnectorError::Unauthorized) => out.fail(CliError::NotLoggedIn),
        Err(e) => out.fail(e),
    }
}

/// Login is never blocked on a list failure.
pub async fn post_login_project_setup(client: &Client, saved_project: Option<&str>, out: Output) {
    let projects: Vec<String> = match client.projects().list().await {
        Ok(res) => res.items.into_iter().map(|i| i.name).collect(),
        Err(_) => return,
    };
    match decide_project_onboarding(&projects, saved_project) {
        ProjectOnboarding::Keep(_) => {}
        ProjectOnboarding::AutoSelect(name) => {
            set_value(ConfigKey::SelectedProject, name.clone(), true);
            out.human(format!("Selected project {name}."));
        }
        ProjectOnboarding::Choose(names) if is_interactive(out) => {
            out.human("You belong to multiple projects:");
            match ask_for_choice("Project", &names, out) {
                Some(name) => {
                    set_value(ConfigKey::SelectedProject, name.clone(), true);
                    out.human(format!("Selected project {name}."));
                }
                None => out.human("Select one later with `gradient project select <name>`."),
            }
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
