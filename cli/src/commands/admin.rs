/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::webhook::{
    NoWebhookOwnerArguments, WebhookSubcommands, run_webhook_subcommand,
};
use crate::input::{OnOffSetting, client_from_config};
use crate::output::Output;
use clap::Subcommand;
use connector::api::{AdminTaskDto, WorkerInfo};
use connector::webhooks::WebhookResource;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Print the live state configuration in `services.gradient.state` format. Fields of the credentials file stay empty.
    State {
        /// Output format: nix or json
        #[arg(long, default_value = "nix", value_parser = ["nix", "json"])]
        export_format: String,
    },
    /// Enable or disable instance drain. Workers get no new jobs while drain is on.
    Drain {
        #[arg(value_enum)]
        setting: OnOffSetting,
    },
    /// Start deep garbage collection of the database and storage. Check progress with `gradient administration task show`.
    DeepGc,
    /// Inspect Background Tasks for Administration
    Task {
        #[command(subcommand)]
        cmd: AdministrationTaskCommand,
    },
    /// Manage webhooks for the whole instance
    Webhook {
        #[command(subcommand)]
        cmd: WebhookSubcommands<NoWebhookOwnerArguments>,
    },
    /// Inspect the workers that connect to this server
    Worker {
        #[command(subcommand)]
        cmd: AdministrationWorkerCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdministrationTaskCommand {
    /// List recent admin tasks
    List,
    /// Print one admin task
    Show {
        /// ID for the admin task
        task_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdministrationWorkerCommand {
    /// List connected workers
    List,
    /// Print one connected worker
    Show {
        /// Worker ID
        worker_id: String,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    let admin = client.admin();
    match cmd {
        Commands::State { export_format } => {
            match admin
                .export_state_configuration_as_text(&export_format)
                .await
            {
                Ok(state) => {
                    out.ok(&serde_json::json!({"format": export_format, "state": state}));
                    out.human(state);
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Drain { setting } => {
            match admin
                .set_draining_enabled(setting == OnOffSetting::On)
                .await
            {
                Ok(draining) => {
                    out.ok(&draining);
                    out.human(match draining.draining {
                        true => "Drain is on.",
                        false => "Drain is off.",
                    });
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::DeepGc => match admin.request_deep_gc_round().await {
            Ok(started) => {
                out.ok(&started);
                out.human(format!(
                    "Deep garbage collection {}, task {}",
                    started.status, started.task_id.0
                ));
            }
            Err(e) => out.fail(e),
        },
        Commands::Task {
            cmd: AdministrationTaskCommand::List,
        } => match admin.list_admin_background_tasks().await {
            Ok(tasks) => {
                out.ok(&tasks);
                if tasks.is_empty() {
                    out.human("There are no admin tasks.");
                }
                tasks
                    .iter()
                    .for_each(|task| print_admin_background_task_human_readable_line(task, out));
            }
            Err(e) => out.fail(e),
        },
        Commands::Task {
            cmd: AdministrationTaskCommand::Show { task_id },
        } => match admin.get_admin_task_by_id(&task_id).await {
            Ok(task) => {
                out.ok(&task);
                print_admin_background_task_human_readable_line(&task, out);
                if let Some(error) = &task.error {
                    out.human(error);
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Webhook { cmd } => {
            run_webhook_subcommand(cmd, |_| WebhookResource::Instance, out).await
        }
        Commands::Worker {
            cmd: AdministrationWorkerCommand::List,
        } => match admin.workers().await {
            Ok(workers) => {
                out.ok(&workers);
                if workers.is_empty() {
                    out.human("No workers connected.");
                }
                workers
                    .iter()
                    .for_each(|worker| print_connected_worker_status_line(worker, out));
            }
            Err(e) => out.fail(e),
        },
        Commands::Worker {
            cmd: AdministrationWorkerCommand::Show { worker_id },
        } => match admin.get_connected_worker_by_id(&worker_id).await {
            Ok(worker) => {
                out.ok(&worker);
                print_connected_worker_status_line(&worker, out);
            }
            Err(e) => out.fail(e),
        },
    }
}

fn print_admin_background_task_human_readable_line(task: &AdminTaskDto, out: Output) {
    out.human(format!(
        "{} {} [{}] {}",
        task.id.0, task.kind, task.status, task.created_at
    ));
}

fn print_connected_worker_status_line(worker: &WorkerInfo, out: Output) {
    let draining = if worker.draining { " [Draining]" } else { "" };
    out.human(format!(
        "{} ({}): {} / {} jobs{}",
        worker.id,
        worker.architectures.join(","),
        worker.assigned_job_count,
        worker.max_concurrent_builds,
        draining
    ));
}
