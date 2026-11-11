/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::commands::team::WorkerCapabilityToggles;
use crate::input::{client_from_config, confirm_delete, selected_project};
use crate::output::Output;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::WorkerRegistrationResponse;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Register a new worker under the selected project. The server can generate a worker ID and token, then write the NixOS configuration for the worker machine.
    Register {
        /// Human-readable display name shown in the workers list
        #[arg(short = 'n', long = "display-name")]
        display_name: String,
        /// Optional WebSocket URL the worker is listening on for incoming server connections
        #[arg(short, long)]
        url: Option<String>,
        #[command(flatten)]
        capabilities: WorkerCapabilityToggles,
    },
    /// List all workers registered under the selected project
    List,
    /// Unregister a worker from the selected project
    Delete {
        /// Worker ID to unregister
        #[arg(add = ArgValueCompleter::new(completion::complete_workers))]
        worker_id: String,
        /// Delete without a confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
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
            display_name,
            url,
            capabilities,
        } => {
            let project = selected_project(out);
            let client = client_from_config(out);
            match client
                .workers()
                .create(
                    &project,
                    capabilities.build_register_worker_request(display_name, url),
                )
                .await
            {
                Ok(registered) => print_worker_registration_result_text(&registered, out),
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

        Commands::Delete { worker_id, yes } => {
            let project = selected_project(out);
            if !confirm_delete(
                &format!("Unregister worker {worker_id} from project \"{project}\"?"),
                yes,
                out,
            ) {
                return;
            }

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

pub fn print_worker_registration_result_text(registered: &WorkerRegistrationResponse, out: Output) {
    out.ok(registered);
    out.human("Worker registered.");
    out.human(format!("Worker ID: {}", registered.id));
    out.human("");
    out.human("NixOS Configuration for Worker Machines");
    out.human(&registered.nixos_configuration);
    out.human("Add this line to the peers file on the worker machine.");
    out.human(format!("{}:{}", registered.peer_id, registered.token));
    out.human("Save the peer line as a secret. The token is visible only once.");
}
