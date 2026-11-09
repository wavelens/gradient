/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::{client_from_config, selected_project};
use crate::output::Output;
use clap::{Args, Subcommand};
use connector::api::{
    CreateWebhookRequest, DeliveryListItemWebhookDeliveryId, UpdateWebhookRequest, WebhookResponse,
};
use connector::webhooks::WebhookResource;

#[derive(Args, Debug)]
pub struct WebhookArgs {
    /// Use the webhooks of this team, not the webhooks of the selected project
    #[arg(long, global = true, value_name = "TEAM")]
    team: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List webhooks with the last delivery result.
    List,
    /// Show a single webhook
    Show {
        /// Webhook ID
        id: uuid::Uuid,
    },
    /// Create a webhook and only print its signing secret, once.
    Create {
        /// Webhook name
        #[arg(short, long)]
        name: String,
        /// URL to receive each event as an HTTP POST
        #[arg(short, long)]
        url: String,
        /// Patterns of events to send (for example `build.failed` or `build.*`). Repeat the flag for multiple patterns. Omit the flag to send all events.
        #[arg(short, long = "event", value_name = "PATTERN")]
        events: Vec<String>,
        /// Send events (true) or pause the webhook (false)
        #[arg(long)]
        active: Option<bool>,
    },
    /// Modify a webhook. Use --event to replace the whole event list.
    Edit {
        /// Webhook ID
        id: uuid::Uuid,
        /// Webhook name
        #[arg(short, long)]
        name: Option<String>,
        /// URL to receive each event as an HTTP POST
        #[arg(short, long)]
        url: Option<String>,
        /// Patterns of events to send (for example `build.failed` or `build.*`). Repeat the flag for multiple patterns. Omit the flag to send all events.
        #[arg(short, long = "event", value_name = "PATTERN")]
        events: Vec<String>,
        /// Send all events and delete all event patterns
        #[arg(long, conflicts_with = "events")]
        all_events: bool,
        /// Send events (true) or pause the webhook (false)
        #[arg(long)]
        active: Option<bool>,
    },
    /// Delete a webhook
    Delete {
        /// Webhook ID
        id: uuid::Uuid,
    },
    /// Replace the webhook signing secret and print the new secret once
    RotateSigningSecret {
        /// Webhook ID
        id: uuid::Uuid,
    },
    /// Send a `webhook.ping` event to the webhook now and print the delivery attempt.
    SendTestPing {
        /// Webhook ID
        id: uuid::Uuid,
    },
    /// Inspect attempts to deliver webhooks
    DeliveryAttempts {
        #[command(subcommand)]
        cmd: WebhookDeliveryAttemptsCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum WebhookDeliveryAttemptsCommand {
    /// List the delivery attempts for a webhook
    List {
        /// Webhook ID
        id: uuid::Uuid,
        /// Maximum number of delivery attempts to list
        #[arg(long)]
        limit: Option<u64>,
        /// Number of delivery attempts to skip
        #[arg(long)]
        offset: Option<u64>,
    },
    /// Show one delivery attempt, including the request body and response body.
    Show {
        /// Webhook ID
        id: uuid::Uuid,
        /// Delivery attempt ID
        delivery_id: uuid::Uuid,
    },
}

fn webhook_owner_from_team_flag_or_selected_project(
    team: Option<String>,
    out: Output,
) -> WebhookResource {
    match team {
        Some(team) => WebhookResource::Team(team),
        None => WebhookResource::Project(selected_project(out)),
    }
}

fn print_webhook_line(out: Output, webhook: &WebhookResponse) {
    let state = if webhook.active { "active" } else { "paused" };
    let events = if webhook.events.is_empty() {
        "*".to_owned()
    } else {
        webhook.events.join(",")
    };
    let last = match &webhook.last_delivery {
        None => "No delivery yet".to_owned(),
        Some(last) => {
            let result = if last.success { "ok" } else { "failed" };
            let status = last
                .response_status
                .map(|status| format!(" {status}"))
                .unwrap_or_default();
            let error = last
                .error_message
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default();
            format!("{result}{status} at {}{error}", last.delivered_at)
        }
    };
    out.human(format!(
        "{} {} -> {} [{state}] events {events}, last {last}",
        webhook.id, webhook.name, webhook.url
    ));
}

fn print_webhook_delivery_attempt_line(out: Output, attempt: &DeliveryListItemWebhookDeliveryId) {
    let result = if attempt.success { "ok" } else { "failed" };
    let status = attempt
        .response_status
        .map_or("-".to_owned(), |status| status.to_string());
    let error = attempt
        .error_message
        .as_deref()
        .map(|error| format!(": {error}"))
        .unwrap_or_default();
    out.human(format!(
        "{} {} {result} {status} {}ms at {}{error}",
        attempt.id, attempt.event, attempt.duration_ms, attempt.delivered_at
    ));
}

pub async fn handle(args: WebhookArgs, out: Output) {
    let owner = webhook_owner_from_team_flag_or_selected_project(args.team, out);
    let client = client_from_config(out);
    let webhooks = client.webhooks();
    match args.cmd {
        Commands::List => match webhooks.list(&owner).await {
            Ok(list) => {
                out.ok(&list);
                if list.is_empty() {
                    out.human("No webhooks.");
                }
                list.iter()
                    .for_each(|webhook| print_webhook_line(out, webhook));
            }
            Err(e) => out.fail(e),
        },

        Commands::Show { id } => match webhooks.get(&owner, &id.to_string()).await {
            Ok(webhook) => {
                out.ok(&webhook);
                print_webhook_line(out, &webhook);
            }
            Err(e) => out.fail(e),
        },

        Commands::Create {
            name,
            url,
            events,
            active,
        } => {
            let body = CreateWebhookRequest {
                name,
                url,
                events,
                active,
            };
            match webhooks.create(&owner, body).await {
                Ok(created) => {
                    out.ok(&created);
                    out.human("Created webhook.");
                    print_webhook_line(out, &created.webhook);
                    out.human(format!("Signing secret: {}", created.secret));
                    out.human(
                        "Keep the signing secret safe. There's no way to display the secret again.",
                    );
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Edit {
            id,
            name,
            url,
            events,
            all_events,
            active,
        } => {
            let events = (all_events || !events.is_empty()).then_some(events);
            let body = UpdateWebhookRequest {
                name,
                url,
                events,
                active,
            };
            match webhooks.update(&owner, &id.to_string(), body).await {
                Ok(webhook) => {
                    out.ok(&webhook);
                    out.human("Updated webhook.");
                    print_webhook_line(out, &webhook);
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete { id } => match webhooks.delete(&owner, &id.to_string()).await {
            Ok(deleted) => {
                out.ok(&deleted);
                out.human("Webhook deleted.");
            }
            Err(e) => out.fail(e),
        },

        Commands::RotateSigningSecret { id } => {
            match webhooks
                .rotate_webhook_signing_secret(&owner, &id.to_string())
                .await
            {
                Ok(rotated) => {
                    out.ok(&rotated);
                    out.human(format!("Signing secret: {}", rotated.secret));
                    out.human(
                        "Keep the signing secret safe. There's no way to display the secret again.",
                    );
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::SendTestPing { id } => {
            match webhooks
                .send_webhook_test_ping(&owner, &id.to_string())
                .await
            {
                Ok(attempt) => {
                    out.ok(&attempt);
                    print_webhook_delivery_attempt_line(out, &attempt);
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::DeliveryAttempts {
            cmd: WebhookDeliveryAttemptsCommand::List { id, limit, offset },
        } => match webhooks
            .list_webhook_delivery_attempts(&owner, &id.to_string(), limit, offset)
            .await
        {
            Ok(attempts) => {
                out.ok(&attempts);
                if attempts.is_empty() {
                    out.human("No delivery attempts.");
                }
                attempts
                    .iter()
                    .for_each(|attempt| print_webhook_delivery_attempt_line(out, attempt));
            }
            Err(e) => out.fail(e),
        },

        Commands::DeliveryAttempts {
            cmd: WebhookDeliveryAttemptsCommand::Show { id, delivery_id },
        } => match webhooks
            .get_webhook_delivery_attempt(&owner, &id.to_string(), &delivery_id.to_string())
            .await
        {
            Ok(attempt) => {
                out.ok(&attempt);
                print_webhook_delivery_attempt_line(
                    out,
                    &DeliveryListItemWebhookDeliveryId {
                        id: attempt.id,
                        event: attempt.event,
                        success: attempt.success,
                        response_status: attempt.response_status,
                        error_message: attempt.error_message,
                        duration_ms: attempt.duration_ms,
                        delivered_at: attempt.delivered_at,
                    },
                );
                out.human("");
                out.human(attempt.request_body);
                if let Some(response) = attempt.response_body {
                    out.human("");
                    out.human(response);
                }
            }
            Err(e) => out.fail(e),
        },
    }
}
