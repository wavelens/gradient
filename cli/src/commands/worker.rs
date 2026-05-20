/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::get_request_config;
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use connector::*;
use std::process::exit;

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
        /// generates one and prints it — store it securely, it cannot be retrieved again.
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
        Commands::Register {
            worker_id,
            display_name,
            url,
            token,
        } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    out.err(
                        ExitKind::Usage,
                        "Organization is required. Use `gradient organization select <name>`.",
                    );
                }
            };

            let token_provided = token.is_some();

            let res = workers::post_org_worker(
                get_request_config(load_config()).unwrap(),
                organization,
                worker_id,
                display_name,
                url,
                token,
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, "Worker registration failed");
            }

            out.human("Worker registered.");
            out.human(format!("Peer ID:  {}", res.message.peer_id));
            if let Some(token) = res.message.token {
                out.human(format!("Token:    {}", token));
                out.human("");
                out.human("Store the token securely — it cannot be retrieved again.");
            } else if token_provided {
                out.human("Token was pre-supplied; not echoed back.");
            }
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    out.err(
                        ExitKind::Usage,
                        "Organization is required. Use `gradient organization select <name>`.",
                    );
                }
            };

            let res =
                workers::get_org_workers(get_request_config(load_config()).unwrap(), organization)
                    .await
                    .map_err(|e| {
                        out.progress(format!("{}", e));
                        exit(1);
                    })
                    .unwrap();

            if res.error {
                out.err(ExitKind::Api, "Failed to list workers");
            }

            if res.message.is_empty() {
                out.human("No workers registered.");
            } else {
                for w in res.message {
                    let status = if w.live.is_some() { "online" } else { "offline" };
                    let url_part = w.url.map(|u| format!(" (url: {})", u)).unwrap_or_default();
                    out.human(format!(
                        "{} \"{}\": {} [{}]{}",
                        w.worker_id, w.display_name, w.registered_at, status, url_part
                    ));
                }
            }
        }

        Commands::Delete { worker_id } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    out.err(
                        ExitKind::Usage,
                        "Organization is required. Use `gradient organization select <name>`.",
                    );
                }
            };

            let res = workers::delete_org_worker(
                get_request_config(load_config()).unwrap(),
                organization,
                worker_id,
            )
            .await
            .map_err(|e| {
                out.progress(format!("{}", e));
                exit(1);
            })
            .unwrap();

            if res.error {
                out.err(ExitKind::Api, "Worker deletion failed");
            }

            out.human("Worker unregistered.");
        }
    }
}
