/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;
use connector::workers::MakeWorkerRequest;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Register a new worker under the selected organization
    Register {
        /// Persistent worker identity string
        worker_id: String,
        /// Human-readable display name shown in the workers list
        #[arg(short = 'n', long = "display-name")]
        display_name: String,
        /// Optional WebSocket URL where the worker listens for incoming server connections
        #[arg(short, long)]
        url: Option<String>,
        /// Pre-generated token (output of `openssl rand -base64 48`). When omitted the server
        /// generates one and prints it - store it securely, it cannot be retrieved again.
        #[arg(short, long)]
        token: Option<String>,
    },
    /// List all workers registered under the selected organization
    List,
    /// Unregister a worker from the selected organization
    Delete {
        /// Worker ID to unregister
        worker_id: String,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Register { worker_id, display_name, url, token } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => out.err(
                    ExitKind::Usage,
                    "Organization is required. Use `gradient organization select <name>`.",
                ),
            };

            let token_provided = token.is_some();
            let client = client_from_config(out);

            match client.workers().create(&organization, MakeWorkerRequest {
                worker_id,
                display_name,
                url,
                token,
                enable_fetch: None,
                enable_eval: None,
                enable_build: None,
            }).await {
                Ok(resp) => {
                    out.ok(&resp);
                    out.human("Worker registered.");
                    out.human(format!("Peer ID:  {}", resp.peer_id));
                    if let Some(tok) = resp.token {
                        out.human(format!("Token:    {}", tok));
                        out.human("");
                        out.human("Store the token securely - it cannot be retrieved again.");
                    } else if token_provided {
                        out.human("Token was pre-supplied; not echoed back.");
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => out.err(
                    ExitKind::Usage,
                    "Organization is required. Use `gradient organization select <name>`.",
                ),
            };

            let client = client_from_config(out);
            match client.workers().list(&organization).await {
                Ok(workers) => {
                    out.ok(&workers);
                    if workers.is_empty() {
                        out.human("No workers registered.");
                    } else {
                        for w in workers {
                            let status = if w.live.is_some() { "online" } else { "offline" };
                            let url_part = w.url.map(|u| format!(" (url: {})", u)).unwrap_or_default();
                            out.human(format!(
                                "{} \"{}\": {} [{}]{}",
                                w.worker_id, w.display_name, w.registered_at, status, url_part
                            ));
                        }
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Delete { worker_id } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => out.err(
                    ExitKind::Usage,
                    "Organization is required. Use `gradient organization select <name>`.",
                ),
            };

            let client = client_from_config(out);
            match client.workers().delete(&organization, &worker_id).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Worker unregistered.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
    }
}
