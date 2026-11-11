/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output};
use clap::{Args, Subcommand};
use connector::Client;
use connector::api::{
    CreateGitHostConnectionRequest, GitHostConnectionId, GitHostConnectionResponse,
    GitHostConnectionSummary, IncomingWebhookDeliveryResponse, IndexName,
    PatchGitHostConnectionRequest,
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
    List,
    /// Create a connection to a Git host. One-time display of the generated webhook secret.
    Create {
        /// Connection Name
        #[arg(short, long)]
        name: String,
        /// Types of Git Host
        #[arg(long, value_parser = GIT_HOST_TYPES)]
        host_type: String,
        #[command(flatten)]
        settings: GitHostConnectionArgs,
    },
    /// Display one connection to a Git host.
    Show {
        /// Connection name or UUID
        connection: String,
    },
    /// Change the connection to the Git host. Flags that aren't specified keep their current values.
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
    Delete {
        /// Connection name or UUID
        connection: String,
    },
    /// List the latest 50 webhook deliveries that reached the connection, newest first.
    Deliveries {
        /// Connection name or UUID
        connection: String,
    },
}

fn on_off(active: bool) -> &'static str {
    if active { "on" } else { "off" }
}

pub fn print_git_host_connection_line(out: Output, connection: &GitHostConnectionSummary) {
    out.human(format!(
        "{} ({}) {} {}, receives events {}, reports status {}: {}",
        connection.name,
        connection.display_name,
        connection.git_host_type,
        GitHostConnectionSetOwner::from(&connection.owner),
        on_off(connection.receives_events),
        on_off(connection.reports_status),
        connection.id
    ));
}

fn print_git_host_connection_fields(out: Output, connection: &GitHostConnectionResponse) {
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
    if !connection.allowed_ips.is_empty() {
        out.human(format!(
            "Allowed IP addresses: {}",
            connection.allowed_ips.join(", ")
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
        yes_no(connection.has_secret)
    ));
    out.human(format!(
        "Access token setting: {}",
        yes_no(connection.has_access_token)
    ));
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
        "{} {} {} {}{repository}{detail}",
        delivery.created_at, delivery.event, delivery.status_code, delivery.result
    ));
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
            settings,
        } => {
            let body = CreateGitHostConnectionRequest {
                name: IndexName(name),
                git_host_type: host_type,
                allowed_ips: settings.allowed_ips(),
                display_name: settings.display_name,
                base_url: settings.base_url,
                access_token: settings.access_token,
                secret: settings.secret,
                receives_events: settings.receives_events,
                reports_status: settings.reports_status,
                installation_id: None,
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
                name: name.map(IndexName),
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
    }
}
