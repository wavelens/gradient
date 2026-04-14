/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::get_request_config;
use clap::Subcommand;
use connector::*;
use std::process::exit;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Register a new worker under the selected organization
    Register {
        /// Persistent worker identity string
        worker_id: String,
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

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Register { worker_id, url, token } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required. Use `gradient organization select <name>`.");
                    exit(1);
                }
            };

            let token_provided = token.is_some();

            let res = workers::post_org_worker(
                get_request_config(load_config()).unwrap(),
                organization,
                worker_id,
                url,
                token,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Worker registration failed");
                exit(1);
            }

            println!("Worker registered.");
            println!("Peer ID:  {}", res.message.peer_id);
            if let Some(token) = res.message.token {
                println!("Token:    {}", token);
                println!();
                println!("Store the token securely — it cannot be retrieved again.");
            } else if token_provided {
                println!("Token was pre-supplied; not echoed back.");
            }
        }

        Commands::List => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required. Use `gradient organization select <name>`.");
                    exit(1);
                }
            };

            let res = workers::get_org_workers(
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
                eprintln!("Failed to list workers");
                exit(1);
            }

            if res.message.is_empty() {
                println!("No workers registered.");
            } else {
                for w in res.message {
                    let status = if w.live.is_some() { "online" } else { "offline" };
                    let url_part = w
                        .url
                        .map(|u| format!(" (url: {})", u))
                        .unwrap_or_default();
                    println!("{}: {} [{}]{}", w.worker_id, w.registered_at, status, url_part);
                }
            }
        }

        Commands::Delete { worker_id } => {
            let organization = match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                Some(id) => id,
                _ => {
                    eprintln!("Organization is required. Use `gradient organization select <name>`.");
                    exit(1);
                }
            };

            let res = workers::delete_org_worker(
                get_request_config(load_config()).unwrap(),
                organization,
                worker_id,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Worker deletion failed");
                exit(1);
            }

            println!("Worker unregistered.");
        }
    }
}
