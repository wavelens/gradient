/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::config::*;
use crate::input::{client_from_config, handle_input, selected_project, selected_task};
use crate::output::{Color, ExitKind, Output};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::{EvaluationStatus, MakeTaskRequest, PatchTaskRequest};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Select the task later commands act on, within the selected project
    Select {
        /// Task name
        #[arg(add = ArgValueCompleter::new(completion::complete_tasks))]
        task: String,
    },
    /// Show the selected task with its last evaluation and builds
    Show,
    /// Stream the logs of the selected task's last evaluation
    Log,
    /// Create a task in the selected project with missing fields opened in $EDITOR, then select it unless `--json` is set
    Create {
        /// Name
        #[arg(short, long)]
        name: Option<String>,
        /// Display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// Description
        #[arg(short = 'c', long)]
        description: Option<String>,
        /// Git repository URL
        #[arg(short, long)]
        repository: Option<String>,
        /// Flake attribute wildcard to evaluate
        #[arg(short = 'w', long)]
        wildcard: Option<String>,
    },
    /// List the tasks of the selected project
    List,
    /// Edit the selected task
    Edit {
        /// New name
        #[arg(short, long)]
        new_name: Option<String>,
        /// New display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// New description
        #[arg(short = 'c', long)]
        description: Option<String>,
        /// New Git repository URL
        #[arg(short, long)]
        repository: Option<String>,
        /// New flake attribute wildcard
        #[arg(short = 'w', long)]
        wildcard: Option<String>,
    },
    /// Delete the selected task
    Delete,
    /// Start an evaluation of the selected task
    Evaluate,
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Select { task } => {
            let project = selected_project(out);

            set_value(
                ConfigKey::SelectedTask,
                format!("{}/{}", project, task),
                true,
            );
            out.ok(&serde_json::json!({"selected": true, "project": project, "task": task}));
            out.human("Task selected in current Project.");
        }

        Commands::Show => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);

            let details = match client.tasks().details(&project, &task).await {
                Ok(d) => d,
                Err(e) => out.fail(e),
            };

            out.ok(&details);
            if out.is_json() {
                return;
            }
            out.human("===== Task =====");
            out.human(format!("Name: {}", details.name));
            out.human(format!("Description: {}", details.description));
            out.human(format!("Repository: {}", details.repository));
            out.human(format!("Wildcard: {}", details.wildcard));
            out.human("");

            if details.last_evaluations.is_empty() {
                out.human("No last evaluation.");
                return;
            }

            let eval_summary = &details.last_evaluations[0];
            let eval = match client.evals().get(&eval_summary.id.to_string()).await {
                Ok(e) => e,
                Err(e) => out.fail(e),
            };

            out.human("===== Evaluation =====");
            out.human(format!("ID: {}", eval.id));
            out.human(format!("Status: {}", eval.status));
            out.human(format!("Commit: {}", eval.commit));
            if let Some(error) = &eval.error {
                out.human(format!("Error: {}", error));
            }
            out.human("");

            let builds = match client.evals().builds(&eval.id.to_string()).await {
                Ok(b) => b,
                Err(e) => out.fail(e),
            };

            if builds.builds.is_empty() {
                out.human("No builds.");
                return;
            }

            out.human("===== Building =====");
            for build in &builds.builds {
                let color = match build.status.as_str() {
                    "Completed" => Color::Green,
                    "Building" | "Running" => Color::Yellow,
                    "Queued" | "Pending" => Color::White,
                    "Failed" | "Error" => Color::Red,
                    _ => Color::Plain,
                };
                out.human(out.paint(&build.name, color));
            }
            out.human("");

            if eval.status != EvaluationStatus::Aborted {
                out.human("===== Log =====");
                crate::commands::logstream::stream_eval_logs(&client, &eval.id.to_string(), out)
                    .await;
            }
        }

        Commands::Log => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            let details = match client.tasks().details(&project, &task).await {
                Ok(d) => d,
                Err(e) => out.fail(e),
            };
            let Some(latest) = details.last_evaluations.first() else {
                out.err(ExitKind::Api, "Task has no evaluations yet.");
            };
            crate::commands::logstream::stream_eval_logs(&client, &latest.id.to_string(), out)
                .await;
        }

        Commands::Create {
            name,
            display_name,
            description,
            repository,
            wildcard,
        } => {
            let project = selected_project(out);

            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
                ("Repository", repository),
                ("Wildcard", wildcard),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true, out);
            let name = input.get("Name");

            let client = client_from_config(out);
            match client
                .tasks()
                .create(
                    &project,
                    MakeTaskRequest {
                        name: name.clone().into(),
                        display_name: input.get("Display Name").into(),
                        description: input.get("Description"),
                        repository: input.get("Repository"),
                        wildcard: input.get("Wildcard"),
                        concurrency: None,
                        retry_failed_builds: None,
                        multi_node_evaluation: None,
                        sign_cache: None,
                        wait_for_workers: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    if !out.is_json() {
                        set_value(ConfigKey::SelectedTask, format!("{project}/{name}"), true);
                    }
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Task created.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::List => {
            let project = selected_project(out);

            let client = client_from_config(out);
            match client.tasks().list(&project).await {
                Ok(res) => {
                    out.ok(&res);
                    if res.items.is_empty() {
                        out.human("You have no tasks.");
                    } else {
                        for task in res.items {
                            out.human(format!("{}: {}", task.name, task.id));
                        }
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Edit {
            new_name,
            display_name,
            description,
            repository,
            wildcard,
        } => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            let current = match client.tasks().details(&project, &task).await {
                Ok(d) => d,
                Err(e) => out.fail(e),
            };

            let input_fields = [
                ("Name", Some(new_name.unwrap_or(current.name))),
                (
                    "Display Name",
                    Some(display_name.unwrap_or(current.display_name)),
                ),
                (
                    "Description",
                    Some(description.unwrap_or(current.description)),
                ),
                ("Repository", Some(repository.unwrap_or(current.repository))),
                ("Wildcard", Some(wildcard.unwrap_or(current.wildcard))),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false, out);

            match client
                .tasks()
                .update(
                    &project,
                    &task,
                    PatchTaskRequest {
                        name: (input.get("Name") != task).then(|| input.get("Name").into()),
                        display_name: Some(input.get("Display Name")),
                        description: Some(input.get("Description")),
                        repository: Some(input.get("Repository")),
                        wildcard: Some(input.get("Wildcard")),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(_) => {
                    let renamed = input.get("Name");
                    if renamed != task {
                        let old = format!("{project}/{task}");
                        update_or_create_saved_login_in_config_file(|login| {
                            if login.selected_task.as_deref() == Some(old.as_str()) {
                                login.selected_task = Some(format!("{project}/{renamed}"));
                            }
                        });
                    }
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Task updated.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            match client.tasks().delete(&project, &task).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Task deleted.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Evaluate => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            match client.tasks().evaluate(&project, &task, None).await {
                Ok(eval_id) => {
                    out.ok(&serde_json::json!({"evaluation_id": eval_id}));
                    out.human("Task evaluation started.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}
