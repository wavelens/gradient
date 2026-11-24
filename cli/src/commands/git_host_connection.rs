/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::mcp_command_run_decisions::{
    McpToolCommandPermission::McpToolMayRunCommand,
    UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES,
};
use crate::input::{client_from_config, selected_project};
use crate::output::{ExitKind, Output};
use clap::{ArgGroup, Args, Subcommand};
use connector::Client;
use connector::api::{
    CreateGitHostConnectionRequest, CreateRepositoryGrantRequest, GitHostConnectionId,
    GitHostConnectionOwner, GitHostConnectionRepositoryGrantId, GitHostConnectionResponse,
    GitHostConnectionSummary, IncomingWebhookDeliveryResponse, IndexName,
    PatchGitHostConnectionRequest, RepositoryGrantResponse,
};
use connector::git_host_connections::GitHostConnectionSetOwner;

const GIT_HOST_TYPES: [&str; 4] = ["gitea", "forgejo", "gitlab", "github"];

#[derive(Args, Debug)]
pub struct GitHostConnectionArgs {
    /// Display name
    #[arg(short, long)]
    display_name: Option<String>,
    /// Base URL of the Git host (for example: `https://git.example.com`). Set an empty value to clear the URL.
    #[arg(long)]
    base_url: Option<String>,
    /// Access token for Gradient to post commit statuses and install webhooks and deploy keys. The flag value can remain in shell history. Use an empty value to clear the flag value.
    #[arg(long)]
    access_token: Option<String>,
    /// Secret for signing incoming webhooks. The server can generate one when this flag is absent at creation time.
    #[arg(long)]
    secret: Option<String>,
    /// Specify a comma-separated list of IP addresses or CIDR blocks that can send webhooks. Use an empty value to allow all addresses.
    #[arg(long, value_delimiter = ',')]
    allowed_ips: Option<Vec<String>>,
    /// Accept webhook events from the Git host: true or false
    #[arg(long)]
    receives_events: Option<bool>,
    /// Post commit status to Git host: true or false
    #[arg(long)]
    reports_status: Option<bool>,
}

impl GitHostConnectionArgs {
    fn allowed_ips(&self) -> Option<Vec<String>> {
        self.allowed_ips.as_ref().map(|ips| {
            ips.iter()
                .map(|ip| ip.trim().to_owned())
                .filter(|ip| !ip.is_empty())
                .collect()
        })
    }
}

#[derive(Subcommand, Debug)]
pub enum GitHostConnectionSubcommands {
    /// List connections to Git hosts that the owner can use.
    #[command(add = McpToolMayRunCommand)]
    List,
    /// Create a connection to a Git host. One-time display of the generated webhook secret.
    #[command(add = UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES)]
    Create {
        /// Connection Name
        #[arg(short, long)]
        name: String,
        /// Types of Git Host
        #[arg(long, value_parser = GIT_HOST_TYPES)]
        host_type: String,
        /// GitHub App installation ID. The number at the end of the installation settings URL on GitHub. Required for `--host-type github`, which can only work with `--owner instance`.
        #[arg(long, required_if_eq("host_type", "github"))]
        installation_id: Option<i64>,
        #[command(flatten)]
        settings: GitHostConnectionArgs,
    },
    /// Display one connection to a Git host.
    #[command(add = McpToolMayRunCommand)]
    Show {
        /// Connection name or UUID
        connection: String,
    },
    /// Change the connection to the Git host. Flags that aren't specified keep their current values. Changes to `--host-type` or to the scheme, host or port of `--base-url` delete the stored access token, OAuth refresh token and webhook secret. A new `--access-token` and a new `--secret` in the same edit remain stored. A new access token does not keep the old webhook secret.
    #[command(add = UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES)]
    Edit {
        /// Connection name or UUID
        connection: String,
        /// New connection name
        #[arg(short, long)]
        name: Option<String>,
        /// Types of Git Host
        #[arg(long, value_parser = GIT_HOST_TYPES)]
        host_type: Option<String>,
        #[command(flatten)]
        settings: GitHostConnectionArgs,
    },
    /// Remove the connection to the Git host. Tasks that use the connection stop reporting commit status.
    #[command(add = McpToolMayRunCommand)]
    Delete {
        /// Connection name or UUID
        connection: String,
    },
    /// List the latest 50 webhook deliveries that reached the connection, newest first.
    #[command(add = McpToolMayRunCommand)]
    Deliveries {
        /// Connection name or UUID
        connection: String,
    },
    /// Sign in to the Git host with the stored access token and print the account or the error from the host.
    #[command(add = McpToolMayRunCommand)]
    Test {
        /// Connection name or UUID
        connection: String,
    },
    /// List the tasks that use the connection.
    #[command(add = McpToolMayRunCommand)]
    Tasks {
        /// Connection name or UUID
        connection: String,
    },
    /// Manage which projects can use this instance connection or team connection on which repositories.
    RepositoryGrants {
        /// Connection name or UUID
        connection: String,
        #[command(subcommand)]
        cmd: GitHostConnectionRepositoryGrantSubcommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum GitHostConnectionRepositoryGrantSubcommands {
    /// List the repository grants of a connection.
    #[command(add = McpToolMayRunCommand)]
    List,
    /// Tasks in the selected project can use this connection on one repository or on all repositories of an owner. The command can return an error for an existing grant.
    #[command(add = McpToolMayRunCommand)]
    #[command(group(
        ArgGroup::new("repository_selection_group")
            .required(true)
            .args(["repository_name", "all_repositories"])
    ))]
    Create {
        /// Owner of a repository on a Git host (for example, an organization)
        #[arg(long)]
        repository_owner: String,
        /// Repository name on the Git host. Conflicts with `--all-repositories`.
        #[arg(long)]
        repository_name: Option<String>,
        /// Apply to all repositories of the owner, not one repository.
        #[arg(long)]
        all_repositories: bool,
    },
    /// Delete a repository grant. Tasks in that project can no longer use connections on that repository.
    #[command(add = McpToolMayRunCommand)]
    Delete {
        /// UUID of the repository grant
        repository_grant_uuid: GitHostConnectionRepositoryGrantId,
    },
}

fn on_off(active: bool) -> &'static str {
    if active { "on" } else { "off" }
}

fn health_of(connection: &GitHostConnectionSummary) -> String {
    let mut parts = Vec::new();
    if let Some(account) = &connection.account_login {
        parts.push(format!("account {account}"));
    }
    if let Some(delivery) = &connection.last_delivery {
        parts.push(format!("last event {} {}", delivery.event, delivery.result));
    }
    if connection.refused_signature_count > 0 {
        parts.push(format!(
            "{} refused signatures",
            connection.refused_signature_count
        ));
    }
    match connection.task_count {
        0 => {}
        1 => parts.push("1 task".to_owned()),
        count => parts.push(format!("{count} tasks")),
    }
    if connection.managed {
        parts.push("declarative state".to_owned());
    }
    parts.iter().map(|part| format!(", {part}")).collect()
}

pub fn print_git_host_connection_line(out: Output, connection: &GitHostConnectionSummary) {
    out.human(format!(
        "{} ({}) {} {}, receives events {}, reports status {}{}: {}",
        connection.name,
        connection.display_name,
        connection.git_host_type,
        GitHostConnectionSetOwner::from(&connection.owner),
        on_off(connection.receives_events),
        on_off(connection.reports_status),
        health_of(connection),
        connection.id
    ));
}

fn print_git_host_connection_fields(
    out: Output,
    git_host_connection_response: &GitHostConnectionResponse,
) {
    let connection = &git_host_connection_response.connection;
    let yes_no = |set: bool| if set { "yes" } else { "no" };
    out.human(format!(
        "{} ({}): {}",
        connection.name, connection.display_name, connection.id
    ));
    out.human(format!(
        "Owner: {}",
        GitHostConnectionSetOwner::from(&connection.owner)
    ));
    out.human(format!("Type of Git host: {}", connection.git_host_type));
    if let Some(url) = &connection.base_url {
        out.human(format!("Base URL: {url}"));
    }
    if let Some(url) = &connection.webhook_url {
        out.human(format!("Webhook URL: {url}"));
    }
    if !git_host_connection_response.allowed_ips.is_empty() {
        out.human(format!(
            "Allowed IP addresses: {}",
            git_host_connection_response.allowed_ips.join(", ")
        ));
    }
    out.human(format!(
        "Event received: {}",
        on_off(connection.receives_events)
    ));
    out.human(format!(
        "Report status: {}",
        on_off(connection.reports_status)
    ));
    out.human(format!(
        "Webhook secret configured: {}",
        yes_no(git_host_connection_response.has_secret)
    ));
    out.human(format!(
        "Access token setting: {}",
        yes_no(git_host_connection_response.has_access_token)
    ));
    if let Some(account) = &connection.account_login {
        out.human(format!("Account: {account}"));
    }
    if let Some(expires_at) = &connection.access_token_expires_at {
        out.human(format!("Access token expires: {expires_at:?}"));
    }
    if let Some(delivery) = &connection.last_delivery {
        out.human("Last event");
        print_incoming_webhook_delivery_line(out, delivery);
    }
    if let Some(refused_at) = &connection.last_refused_signature_at {
        out.human(format!(
            "Refused signatures: {}, last at {refused_at:?}",
            connection.refused_signature_count
        ));
    }
    if let Some(post) = &connection.last_status_post {
        let result = post
            .error_message
            .as_deref()
            .map_or_else(|| "ok".to_owned(), |error| format!("failed: {error}"));
        out.human(format!(
            "Last status post: {:?} {result}",
            post.delivered_at
        ));
    }
    out.human(format!("Tasks that use this: {}", connection.task_count));
    if connection.managed {
        out.human(
            "This connection is under declarative state management. Edits from the CLI fail.",
        );
    }
}

pub fn print_incoming_webhook_delivery_line(
    out: Output,
    delivery: &IncomingWebhookDeliveryResponse,
) {
    let repository = delivery
        .repository
        .as_deref()
        .map(|repository| format!(" {repository}"))
        .unwrap_or_default();
    let detail = delivery
        .detail
        .as_deref()
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default();
    out.human(format!(
        "{:?} {} {} {}{repository}{detail}",
        delivery.created_at, delivery.event, delivery.status_code, delivery.result
    ));
}

fn print_repository_grant_line(out: Output, grant: &RepositoryGrantResponse) {
    let repository_name = grant
        .repository_name
        .as_deref()
        .unwrap_or("* (all repositories)");
    let project = grant
        .project_name
        .clone()
        .unwrap_or_else(|| grant.project.to_string());
    out.human(format!(
        "{}/{repository_name} {project} {}: {}",
        grant.repository_owner, grant.source, grant.id.0
    ));
}

fn exit_with_usage_error_if_repository_grant_connection_owner_is_project(
    owner: &GitHostConnectionSetOwner,
    out: Output,
) {
    if matches!(owner, GitHostConnectionSetOwner::Project(_)) {
        out.err(
            ExitKind::Usage,
            "Repository grants exist only for instance connections and team connections. Use `--owner instance` or `--owner team/<name>`.",
        );
    }
}

async fn run_repository_grant_subcommand_for_resolved_connection(
    cmd: GitHostConnectionRepositoryGrantSubcommands,
    client: &Client,
    owner: &GitHostConnectionSetOwner,
    id: &str,
    out: Output,
) {
    let connections = client.git_host_connections();
    match cmd {
        GitHostConnectionRepositoryGrantSubcommands::List => {
            let grants = connections
                .list_repository_grants_for_git_host_connection(owner, id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&grants);
            if grants.is_empty() {
                out.human("No repository grants.");
            }
            grants
                .iter()
                .for_each(|grant| print_repository_grant_line(out, grant));
        }

        GitHostConnectionRepositoryGrantSubcommands::Create {
            repository_owner,
            repository_name,
            all_repositories: _,
        } => {
            let body = CreateRepositoryGrantRequest {
                project: selected_project(out),
                repository_owner,
                repository_name,
            };
            let created = connections
                .create_repository_grant(owner, id, &body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&created);
            out.human("Created a repository grant.");
            print_repository_grant_line(out, &created);
        }

        GitHostConnectionRepositoryGrantSubcommands::Delete {
            repository_grant_uuid,
        } => {
            let deleted = connections
                .delete_repository_grant(owner, id, &repository_grant_uuid)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deleted);
            out.human("Removed the repository grant.");
        }
    }
}

async fn resolve_git_host_connection_uuid_from_name_or_uuid(
    client: &Client,
    owner: &GitHostConnectionSetOwner,
    reference: &str,
    out: Output,
) -> String {
    if reference.parse::<GitHostConnectionId>().is_ok() {
        return reference.to_owned();
    }
    client
        .git_host_connections()
        .list(owner)
        .await
        .unwrap_or_else(|e| out.fail(e))
        .into_iter()
        .find(|connection| {
            connection.name == reference
                && GitHostConnectionSetOwner::from(&connection.owner) == *owner
        })
        .map(|connection| connection.id.to_string())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!("There is no connection for {owner} to a Git host named '{reference}'."),
            )
        })
}

pub async fn resolve_git_host_connection_available_to_project(
    client: &Client,
    project: &str,
    reference: &str,
    out: Output,
) -> GitHostConnectionId {
    if let Ok(id) = reference.parse::<GitHostConnectionId>() {
        return id;
    }
    let closest_owner_first = |connection: &GitHostConnectionSummary| match connection.owner {
        GitHostConnectionOwner::Project { .. } => 0,
        GitHostConnectionOwner::Team { .. } => 1,
        GitHostConnectionOwner::Instance => 2,
    };
    client
        .git_host_connections()
        .list(&GitHostConnectionSetOwner::Project(project.to_owned()))
        .await
        .unwrap_or_else(|e| out.fail(e))
        .into_iter()
        .filter(|connection| connection.name == reference)
        .min_by_key(closest_owner_first)
        .map(|connection| connection.id)
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!(
                    "Project {project} can't use the Git host connection named '{reference}'. Run `gradient project git-host-connection list` to list the available connections."
                ),
            )
        })
}

pub async fn run_git_host_connection_subcommand(
    cmd: GitHostConnectionSubcommands,
    owner: GitHostConnectionSetOwner,
    out: Output,
) {
    let client = client_from_config(out);
    let connections = client.git_host_connections();

    match cmd {
        GitHostConnectionSubcommands::List => {
            let list = connections
                .list(&owner)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&list);
            if list.is_empty() {
                out.human("No connection to the Git host.");
            }
            list.iter()
                .for_each(|connection| print_git_host_connection_line(out, connection));
        }

        GitHostConnectionSubcommands::Create {
            name,
            host_type,
            installation_id,
            settings,
        } => {
            let body = CreateGitHostConnectionRequest {
                name: IndexName::try_from(name).unwrap_or_else(|e| out.fail(e)),
                git_host_type: host_type,
                allowed_ips: settings.allowed_ips(),
                display_name: settings.display_name,
                base_url: settings.base_url,
                access_token: settings.access_token,
                secret: settings.secret,
                receives_events: settings.receives_events.unwrap_or(true),
                reports_status: settings.reports_status.unwrap_or_default(),
                installation_id,
            };
            let created = connections
                .create(&owner, &body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&created);
            out.human("Created a connection to the Git host.");
            print_git_host_connection_fields(out, &created);
            if let Some(secret) = &created.generated_secret {
                out.human(format!("Webhook secret: {secret}"));
                out.human(
                    "Store the webhook secret in a safe place. There's no way to show it again. Enter it in the webhook settings of the Git host.",
                );
            }
        }

        GitHostConnectionSubcommands::Show { connection } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let connection = connections
                .get(&owner, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&connection);
            print_git_host_connection_fields(out, &connection);
        }

        GitHostConnectionSubcommands::Edit {
            connection,
            name,
            host_type,
            settings,
        } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let body = PatchGitHostConnectionRequest {
                name: name.map(|name| IndexName::try_from(name).unwrap_or_else(|e| out.fail(e))),
                git_host_type: host_type,
                allowed_ips: settings.allowed_ips(),
                display_name: settings.display_name,
                base_url: settings.base_url,
                access_token: settings.access_token,
                secret: settings.secret,
                receives_events: settings.receives_events,
                reports_status: settings.reports_status,
            };
            let updated = connections
                .update(&owner, &id, &body)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&updated);
            out.human("Updated the connection to the Git host.");
            print_git_host_connection_fields(out, &updated);
            if let Some(secret) = &updated.generated_secret {
                out.human(format!("Webhook secret: {secret}"));
                out.human(
                    "Store the webhook secret in a safe place. There's no way to show it again. Enter it in the webhook settings of the Git host.",
                );
            }
        }

        GitHostConnectionSubcommands::Delete { connection } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let deleted = connections
                .delete(&owner, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deleted);
            out.human("Removed the connection to the Git host.");
        }

        GitHostConnectionSubcommands::Deliveries { connection } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let deliveries = connections
                .list_latest_webhook_deliveries_for_git_host_connection(&owner, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&deliveries);
            if deliveries.is_empty() {
                out.human("No webhook deliveries.");
            }
            deliveries
                .iter()
                .for_each(|delivery| print_incoming_webhook_delivery_line(out, delivery));
        }

        GitHostConnectionSubcommands::Test { connection } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let checked = connections
                .check(&owner, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            if !checked.success {
                out.err(
                    ExitKind::Api,
                    checked
                        .error
                        .unwrap_or_else(|| "The Git host rejected the access token.".to_owned()),
                );
            }
            out.ok(&checked);
            match &checked.account {
                Some(account) => out.human(format!("Signed in as {account}.")),
                None => out.human("Git host accepted the access token."),
            }
        }

        GitHostConnectionSubcommands::Tasks { connection } => {
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            let tasks = connections
                .list_tasks(&owner, &id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&tasks);
            if tasks.is_empty() {
                out.human("No tasks use this connection.");
            }
            for task in &tasks {
                out.human(format!(
                    "{}/{} ({}) {}, commit status report {}",
                    task.project,
                    task.name,
                    task.display_name,
                    task.repository,
                    on_off(task.report_status)
                ));
            }
        }

        GitHostConnectionSubcommands::RepositoryGrants { connection, cmd } => {
            exit_with_usage_error_if_repository_grant_connection_owner_is_project(&owner, out);
            let id = resolve_git_host_connection_uuid_from_name_or_uuid(
                &client,
                &owner,
                &connection,
                out,
            )
            .await;
            run_repository_grant_subcommand_for_resolved_connection(cmd, &client, &owner, &id, out)
                .await;
        }
    }
}
