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
use connector::api::{AdminTaskDto, AdminWorkerListEntry};
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
    /// Manage the workers that are registered on this server.
    Worker {
        #[command(subcommand)]
        cmd: AdministrationWorkerCommand,
    },
    /// Print all server options with their values and the source of each value. Secret values stay hidden.
    Config,
    /// Print free disk space and storage usage for each cache, project, and task.
    Storage,
    /// Start one garbage collection now. Don't wait for the next interval.
    Gc,
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
    /// List registered workers with team, project, and connection state.
    List,
    /// Show one registered worker
    Show {
        /// Worker ID
        worker_id: String,
    },
    /// Drain one worker. Jobs in progress run to completion on this worker. New jobs don't go to this worker.
    Drain {
        /// Worker ID
        worker_id: String,
    },
    /// Stop the drain on one worker and allow it to fetch new jobs again
    Undrain {
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
                    out.human("No workers registered.");
                }
                workers
                    .iter()
                    .for_each(|worker| print_admin_worker_list_entry_line(worker, out));
            }
            Err(e) => out.fail(e),
        },
        Commands::Worker {
            cmd: AdministrationWorkerCommand::Show { worker_id },
        } => match admin.get_registered_worker_by_id(&worker_id).await {
            Ok(worker) => {
                out.ok(&worker);
                print_admin_worker_list_entry_line(&worker, out);
            }
            Err(e) => out.fail(e),
        },
        Commands::Worker {
            cmd: AdministrationWorkerCommand::Drain { worker_id },
        } => set_worker_draining(&admin, &worker_id, true, out).await,
        Commands::Worker {
            cmd: AdministrationWorkerCommand::Undrain { worker_id },
        } => set_worker_draining(&admin, &worker_id, false, out).await,
        Commands::Config => match admin
            .get_admin_config_options_with_values_and_sources()
            .await
        {
            Ok(options) => {
                out.ok(&options);
                for option in &options {
                    out.human(format!(
                        "--{} = {} ({:?})",
                        option.flag,
                        option.value.as_deref().unwrap_or(""),
                        option.source
                    ));
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Storage => match admin.get_admin_storage_disk_usage_and_gc_stats().await {
            Ok(storage) => {
                out.ok(&storage);
                if let (Some(total), Some(free)) =
                    (storage.disk_total_bytes, storage.disk_free_bytes)
                {
                    out.human(format!("Disk: {free} of {total} bytes free"));
                }
                for cache in &storage.caches {
                    out.human(format!("cache {}: {} bytes", cache.name, cache.bytes));
                }
                for project in &storage.projects {
                    out.human(format!(
                        "project {}: {} bytes",
                        project.project.name, project.bytes
                    ));
                }
                for task in &storage.tasks {
                    out.human(format!(
                        "task {}/{}: {} bytes",
                        task.project.name, task.name, task.bytes
                    ));
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Gc => match admin.request_gc_round().await {
            Ok(()) => {
                out.ok(&serde_json::json!({"status": "accepted"}));
                out.human("A round of garbage collection started.");
            }
            Err(e) => out.fail(e),
        },
    }
}

async fn set_worker_draining(
    admin: &connector::admin::AdminApi<'_>,
    worker_id: &str,
    draining: bool,
    out: Output,
) {
    match admin.set_worker_draining_enabled(worker_id, draining).await {
        Ok(worker) => {
            out.ok(&worker);
            print_admin_worker_list_entry_line(&worker, out);
        }
        Err(e) => out.fail(e),
    }
}

fn print_admin_background_task_human_readable_line(task: &AdminTaskDto, out: Output) {
    out.human(format!(
        "{} {} [{}] {}",
        task.id.0, task.kind, task.status, task.created_at
    ));
}

fn print_admin_worker_list_entry_line(worker: &AdminWorkerListEntry, out: Output) {
    let owner = worker
        .team
        .as_ref()
        .map(|team| team.name.clone())
        .unwrap_or_else(|| {
            worker
                .registrations
                .iter()
                .map(|registration| registration.project.name.clone())
                .collect::<Vec<_>>()
                .join(",")
        });
    let state = match &worker.live {
        Some(live) if live.draining => format!(
            "draining, {} / {} jobs",
            live.assigned_job_count, live.max_concurrent_builds
        ),
        Some(live) => format!(
            "{} / {} jobs",
            live.assigned_job_count, live.max_concurrent_builds
        ),
        None => "offline".to_owned(),
    };
    out.human(format!("{} [{}]: {}", worker.worker_id, owner, state));
}
