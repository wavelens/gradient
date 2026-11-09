/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::input::{client_from_config, selected_project};
use crate::output::Output;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::RegisterWorkerRequest;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Register a new worker under the selected project
    Register {
        /// Worker UUID
        worker_id: uuid::Uuid,
        /// Human-readable display name shown in the workers list
        #[arg(short = 'n', long = "display-name")]
        display_name: String,
        /// Optional WebSocket URL the worker is listening on for incoming server connections
        #[arg(short, long)]
        url: Option<String>,
        /// Pre-generated token (output of `openssl rand -base64 48`). The server is generating and
        /// printing one when omitted. Store it securely, it cannot be retrieved again.
        #[arg(short, long)]
        token: Option<String>,
    },
    /// List all workers registered under the selected project
    List,
    /// Unregister a worker from the selected project
    Delete {
        /// Worker ID to unregister
        #[arg(add = ArgValueCompleter::new(completion::complete_workers))]
        worker_id: String,
    },
    /// Print metrics for a worker in the selected project.
    Metrics {
        /// Worker ID
        #[arg(add = ArgValueCompleter::new(completion::complete_workers))]
        worker_id: String,
    },
    /// Check the worker connection and the permission to work on the selected project.
    Check {
        /// Worker ID
        #[arg(add = ArgValueCompleter::new(completion::complete_workers))]
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
            let project = selected_project(out);

            let token_provided = token.is_some();
            let client = client_from_config(out);

            match client
                .workers()
                .create(
                    &project,
                    RegisterWorkerRequest {
                        worker_id: worker_id.to_string(),
                        display_name,
                        url,
                        token,
                        enable_build: None,
                        enable_eval: None,
                        enable_exec: None,
                        enable_network: None,
                        enable_secrets: None,
                    },
                )
                .await
            {
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
                Err(e) => out.fail(e),
            }
        }

        Commands::List => {
            let project = selected_project(out);

            let client = client_from_config(out);
            match client.workers().list(&project).await {
                Ok(workers) => {
                    out.ok(&workers);
                    if workers.is_empty() {
                        out.human("No workers registered.");
                    } else {
                        for w in workers {
                            let status = if w.connected { "online" } else { "offline" };
                            let url_part =
                                w.url.map(|u| format!(" (url: {})", u)).unwrap_or_default();
                            out.human(format!(
                                "{} \"{}\": {} [{}]{}",
                                w.worker_id, w.display_name, w.registered_at, status, url_part
                            ));
                        }
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete { worker_id } => {
            let project = selected_project(out);

            let client = client_from_config(out);
            match client.workers().delete(&project, &worker_id).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Worker unregistered.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Metrics { worker_id } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client
                .workers()
                .get_worker_metrics(&project, &worker_id)
                .await
            {
                Ok(metrics) => {
                    out.ok(&metrics);
                    out.human(format!(
                        "{} \"{}\"",
                        metrics.worker_id, metrics.display_name
                    ));
                    out.human(format!(
                        "Jobs dispatched in total: {}",
                        metrics.jobs_dispatched
                    ));
                    if let Some(sample) = metrics.samples.last() {
                        let cpu = sample
                            .cpu_usage_pct
                            .map_or_else(|| "-".to_owned(), |cpu| format!("{cpu:.0}%"));
                        out.human(format!(
                            "Final sample ({}): {} / {} jobs, CPU {}",
                            sample.at, sample.assigned_jobs, sample.max_concurrent_builds, cpu
                        ));
                    }
                    for connection in &metrics.connections {
                        match &connection.disconnected_at {
                            Some(disconnected_at) => out.human(format!(
                                "Connected at {}. Disconnected at {}.",
                                connection.connected_at, disconnected_at
                            )),
                            None => out.human(format!(
                                "Connected at {}, still online",
                                connection.connected_at
                            )),
                        }
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Check { worker_id } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client
                .workers()
                .test_worker_connection_and_authorization(&project, &worker_id)
                .await
            {
                Ok(result) => {
                    out.ok(&result);
                    out.human(&result.message);
                }
                Err(e) => out.fail(e),
            }
        }
    }
}
