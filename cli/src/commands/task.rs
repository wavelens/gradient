/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::builds::BuildClass;
use crate::commands::{completion, task_subcommands};
use crate::config::*;
use crate::input::{
    CommandLineFormField, client_from_config, confirm_delete, handle_input, selected_project,
    selected_task,
};
use crate::output::{Color, ExitKind, Output};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::Client;
use connector::api::{
    DuplicateTaskRequest, EvaluationStatus, MakeTaskRequest, PatchTaskRequest, TriggerConfig,
};

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
        /// Branch that the triggers evaluate. Default: the default branch of the repository
        #[arg(short, long)]
        branch: Option<String>,
        /// Trigger to add: `polling`, `cron=<expression>`, `push` or `pr`. Repeat this flag to specify multiple triggers. Leave out this flag to set one polling trigger for the task.
        #[arg(long = "trigger", value_parser = parse_trigger_value)]
        triggers: Vec<CommandLineTrigger>,
        /// Create a task without a default trigger for polling.
        #[arg(long)]
        no_default_trigger: bool,
    },
    /// Copy the selected task and its settings, triggers, and actions to a new task for another branch.
    Duplicate {
        /// New task name
        #[arg(short, long)]
        name: String,
        /// Display name for the new task
        #[arg(short, long)]
        display_name: String,
        /// Branch for each trigger of the new task
        #[arg(short, long)]
        branch: String,
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
    Delete {
        /// Delete without a confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Start an evaluation of the selected task
    Evaluate,
    /// Move the selected task to another project.
    Transfer {
        /// Name of the target project
        #[arg(add = ArgValueCompleter::new(completion::complete_projects))]
        project: String,
    },
    /// Manage the connection to the Git repository for the selected task.
    Repository {
        #[command(subcommand)]
        cmd: TaskRepositoryCommands,
    },
    /// Manage actions that run on events of the selected task.
    Action {
        #[command(subcommand)]
        cmd: TaskActionCommands,
    },
    /// Manage flake input overrides for the selected task
    FlakeInput {
        #[command(subcommand)]
        cmd: FlakeInputCommands,
    },
    /// Manage the triggers that start an evaluation of the selected task.
    Trigger {
        #[command(subcommand)]
        cmd: TaskTriggerCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskRepositoryCommands {
    /// Show the repository for the selected task, the connection to the Git host, commit status reports, and the latest webhook events.
    Show,
    /// Select the connection to the Git host for the selected task and turn commit status reports on or off. Flags not specified keep current values.
    SetConnection {
        /// Connection name or UUID from the connections available in `gradient task repository show`
        #[arg(long, conflicts_with = "no_connection")]
        connection: Option<String>,
        /// Remove the connection between the task and the Git host. Commit status reports stop.
        #[arg(long)]
        no_connection: bool,
        /// Post commit status for task evaluation: true or false
        #[arg(long)]
        report_status: Option<bool>,
    },
    /// Post a pending test commit status to the commit of the latest evaluation.
    CheckCommitStatus,
    /// Add the SSH key of the project as a read-only deploy key to the repository on the Git host
    InstallDeployKey,
    /// Create a webhook on the Git host to send repository events to Gradient.
    InstallWebhook,
}

#[derive(Subcommand, Debug)]
pub enum TaskActionCommands {
    /// List the actions for the selected task.
    List,
    /// Show Action
    Show {
        /// Action name or UUID
        action: String,
    },
    /// Create an action. The output can only show the `send_web_request` action token, once.
    Create {
        /// Action Name
        #[arg(short, long)]
        name: String,
        /// Action config is JSON with a `type` field. The JSON `{"type":"send_mail","recipients":["ops@example.com"]}` is an example.
        #[arg(short, long)]
        config: String,
        /// Comma-separated events that trigger the action (e.g. evaluation.completed)
        #[arg(short, long, value_delimiter = ',')]
        events: Vec<String>,
        /// Create an action as inactive
        #[arg(long)]
        inactive: bool,
    },
    /// Change the action. Flags not specified keep their current values.
    Edit {
        /// Action name or UUID
        action: String,
        /// Action Name
        #[arg(short, long)]
        name: Option<String>,
        /// Action config is JSON with a `type` field. The JSON `{"type":"send_mail","recipients":["ops@example.com"]}` is an example.
        #[arg(short, long)]
        config: Option<String>,
        /// Comma-separated events that trigger the action (e.g. evaluation.completed)
        #[arg(short, long, value_delimiter = ',')]
        events: Option<Vec<String>>,
        /// Switch an action on or off: true or false.
        #[arg(long)]
        active: Option<bool>,
    },
    /// Delete action
    Delete {
        /// Action name or UUID
        action: String,
    },
    /// List past deliveries of the action
    Deliveries {
        /// Action name or UUID
        action: String,
        /// Maximum number of entries
        #[arg(long)]
        limit: Option<u64>,
        /// Number of entries to skip
        #[arg(long)]
        offset: Option<u64>,
    },
    /// Show delivery with request body and response body.
    Delivery {
        /// Action name or UUID
        action: String,
        /// Delivery UUID
        delivery: String,
    },
    /// Run the action right now with a test event. Actions for a Git host only check the connection.
    Test {
        /// Action name or UUID
        action: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum FlakeInputCommands {
    /// List the flake input overrides of the selected task
    List,
    /// Display override settings for flake inputs
    Show {
        /// Name of the flake input, or the UUID of the override
        input: String,
    },
    /// Override flake inputs for evaluation of the selected task
    Create {
        /// Name of the flake input (e.g. nixpkgs)
        input_name: String,
        /// Flake reference to use instead (e.g. github:NixOS/nixpkgs/nixos-unstable). Leave this option empty to update the input and preserve the original URL.
        #[arg(long)]
        url: Option<String>,
    },
    /// Change the override for a flake input.
    Edit {
        /// Name of the flake input, or the UUID of the override
        input: String,
        /// Name of the flake input (e.g. nixpkgs)
        #[arg(long)]
        new_name: Option<String>,
        /// Flake reference to use instead (e.g. github:NixOS/nixpkgs/nixos-unstable). Leave this option empty to update the input and preserve the original URL.
        #[arg(long, conflicts_with = "clear_url")]
        url: Option<String>,
        /// Remove the URL and update the input to retain the original URL.
        #[arg(long)]
        clear_url: bool,
    },
    /// Delete flake input overrides
    Delete {
        /// Name of the flake input, or the UUID of the override
        input: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskTriggerCommands {
    /// List the triggers for the selected task.
    List,
    /// Show trigger
    Show {
        /// Trigger UUID
        trigger: String,
    },
    /// Create a trigger
    Create {
        /// Specify the trigger configuration as JSON with a `type` field. An example is '{"type":"time","cron":"0 3 * * *"}'
        #[arg(short, long)]
        config: String,
        /// Create the trigger as inactive.
        #[arg(long)]
        inactive: bool,
    },
    /// Change the trigger. Omitted flags keep their current value.
    Edit {
        /// Trigger UUID
        trigger: String,
        /// Specify the trigger configuration as JSON with a `type` field. An example is '{"type":"time","cron":"0 3 * * *"}'
        #[arg(short, long)]
        config: Option<String>,
        /// Enable or disable the trigger with true or false.
        #[arg(long)]
        active: Option<bool>,
    },
    /// Delete Trigger
    Delete {
        /// Trigger UUID
        trigger: String,
    },
    /// Fire the trigger now against the task's current head commit.
    Test {
        /// Trigger UUID
        trigger: String,
    },
    /// Set the trigger to default. Manual evaluations use that branch.
    Default {
        /// Trigger UUID
        trigger: String,
    },
}

const DEFAULT_POLLING_INTERVAL_SECS: i32 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineTrigger {
    Polling,
    Cron(String),
    Push,
    PullRequest,
}

fn parse_trigger_value(value: &str) -> Result<CommandLineTrigger, String> {
    match value.split_once('=') {
        None if value == "polling" => Ok(CommandLineTrigger::Polling),
        None if value == "push" => Ok(CommandLineTrigger::Push),
        None if value == "pr" => Ok(CommandLineTrigger::PullRequest),
        Some(("cron", expression)) if !expression.trim().is_empty() => {
            Ok(CommandLineTrigger::Cron(expression.trim().to_owned()))
        }
        _ => Err(format!(
            "Unknown trigger '{value}'. Use polling, cron=<expression>, push or pr."
        )),
    }
}

async fn build_optional_task_create_trigger_configs(
    client: &Client,
    project: &str,
    repository: &str,
    branch: Option<String>,
    mut triggers: Vec<CommandLineTrigger>,
    no_default_trigger: bool,
    out: Output,
) -> Option<Vec<TriggerConfig>> {
    if triggers.is_empty() {
        if no_default_trigger {
            return Some(Vec::new());
        }
        branch.as_ref()?;
        triggers.push(CommandLineTrigger::Polling);
    }

    let mut suggestions: Option<Vec<TriggerConfig>> = None;
    let mut configs = Vec::with_capacity(triggers.len());
    for trigger in triggers {
        let config = match trigger {
            CommandLineTrigger::Polling => TriggerConfig::Polling {
                interval_secs: DEFAULT_POLLING_INTERVAL_SECS,
                branch: branch.clone(),
            },
            CommandLineTrigger::Cron(cron) => TriggerConfig::Time {
                cron,
                branch: branch.clone(),
            },
            CommandLineTrigger::Push | CommandLineTrigger::PullRequest => {
                if suggestions.is_none() {
                    suggestions = Some(
                        client
                            .tasks()
                            .get_webhook_trigger_suggestions_for_repository(project, repository)
                            .await
                            .unwrap_or_else(|e| out.fail(e)),
                    );
                }
                let wants_push = trigger == CommandLineTrigger::Push;
                let mut config = suggestions
                    .iter()
                    .flatten()
                    .find(|config| match config {
                        TriggerConfig::ReporterPush { .. } => wants_push,
                        TriggerConfig::ReporterPullRequest { .. } => !wants_push,
                        _ => false,
                    })
                    .cloned()
                    .unwrap_or_else(|| {
                        out.err(
                            ExitKind::Usage,
                            format!(
                                "No connection in this project can receive events from the Git host for {repository}. Add a connection in `gradient project git-host-connection create --receives-events true`. Then try again."
                            ),
                        )
                    });
                if let TriggerConfig::ReporterPush { branches, .. }
                | TriggerConfig::ReporterPullRequest { branches, .. } = &mut config
                {
                    *branches = branch.iter().cloned().collect();
                }
                config
            }
        };
        configs.push(config);
    }
    Some(configs)
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
                let color = match BuildClass::build_class_from_status_text(&build.status) {
                    BuildClass::Succeeded => Color::Green,
                    BuildClass::Building => Color::Yellow,
                    BuildClass::Queued => Color::White,
                    BuildClass::Failed => Color::Red,
                    BuildClass::AbortedSkippedOrUnknown => Color::Plain,
                };
                out.human(out.paint(&build.name, color));
            }
            out.human("");

            if eval.status != EvaluationStatus::Aborted {
                out.human("===== Log =====");
                if let Err(e) =
                    crate::commands::logstream::stream_eval_logs(&client, &eval.id.to_string(), out)
                        .await
                {
                    out.fail(e);
                }
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
            if let Err(e) =
                crate::commands::logstream::stream_eval_logs(&client, &latest.id.to_string(), out)
                    .await
            {
                out.fail(e);
            }
        }

        Commands::Create {
            name,
            display_name,
            description,
            repository,
            wildcard,
            branch,
            triggers,
            no_default_trigger,
        } => {
            let project = selected_project(out);

            let input_fields = vec![
                CommandLineFormField::new_non_empty("Name", name),
                CommandLineFormField::new_non_empty("Display Name", display_name),
                CommandLineFormField::new_optional_form_field("Description", description),
                CommandLineFormField::new_non_empty("Repository", repository),
                CommandLineFormField::new_non_empty("Wildcard", wildcard),
            ];

            let input = handle_input(input_fields, false, out);
            let name = input.get("Name");

            let repository = input.get("Repository");

            let client = client_from_config(out);
            let triggers = build_optional_task_create_trigger_configs(
                &client,
                &project,
                &repository,
                branch,
                triggers,
                no_default_trigger,
                out,
            )
            .await;
            match client
                .tasks()
                .create(
                    &project,
                    MakeTaskRequest {
                        name: name.clone().into(),
                        display_name: input.get("Display Name").into(),
                        description: input.get("Description"),
                        repository,
                        wildcard: input.get("Wildcard"),
                        concurrency: None,
                        retry_failed_builds: None,
                        multi_node_evaluation: None,
                        wait_for_workers: None,
                        exec_enabled: None,
                        triggers,
                    },
                )
                .await
            {
                Ok(created) => {
                    if !out.is_json() {
                        set_value(ConfigKey::SelectedTask, format!("{project}/{name}"), true);
                    }
                    out.ok(&created);
                    out.progress("Task created.");
                    out.human(created.id);
                    crate::commands::project::print_missing_project_prerequisites(
                        &client, &project, out,
                    )
                    .await;
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

            let input_fields = vec![
                CommandLineFormField::new_non_empty("Name", new_name).default_value(current.name),
                CommandLineFormField::new_non_empty("Display Name", display_name)
                    .default_value(current.display_name),
                CommandLineFormField::new_optional_form_field("Description", description)
                    .default_value(current.description),
                CommandLineFormField::new_non_empty("Repository", repository)
                    .default_value(current.repository),
                CommandLineFormField::new_non_empty("Wildcard", wildcard)
                    .default_value(current.wildcard),
            ];

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

        Commands::Duplicate {
            name,
            display_name,
            branch,
        } => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            match client
                .tasks()
                .duplicate_task(
                    &project,
                    &task,
                    &DuplicateTaskRequest {
                        name: name.clone().into(),
                        display_name: display_name.into(),
                        branch,
                    },
                )
                .await
            {
                Ok(created) => {
                    out.ok(&created);
                    out.progress(format!("Task duplicated as {name}."));
                    out.human(created.id);
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete { yes } => {
            let (project, task) = selected_task(out);
            if !confirm_delete(&format!("Delete task '{project}/{task}'?"), yes, out) {
                return;
            }

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
                Ok(created) => {
                    out.ok(&created);
                    out.progress("Task evaluation started.");
                    out.human(created.id);
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Transfer { project: target } => {
            let (project, task) = selected_task(out);

            let client = client_from_config(out);
            match client
                .tasks()
                .move_task_to_project(&project, &task, &target)
                .await
            {
                Ok(_) => {
                    let old = format!("{project}/{task}");
                    update_or_create_saved_login_in_config_file(|login| {
                        if login.selected_task.as_deref() == Some(old.as_str()) {
                            login.selected_task = Some(format!("{target}/{task}"));
                        }
                    });
                    out.ok(&serde_json::json!({"moved": true, "project": target, "task": task}));
                    out.human(format!("Moved task to project {target}."));
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Repository { cmd } => {
            task_subcommands::handle_task_repository_subcommand(cmd, out).await
        }
        Commands::Action { cmd } => {
            task_subcommands::run_gradient_task_action_subcommand(cmd, out).await
        }
        Commands::FlakeInput { cmd } => {
            task_subcommands::run_flake_input_subcommand(cmd, out).await
        }
        Commands::Trigger { cmd } => task_subcommands::run_task_trigger_subcommand(cmd, out).await,
    }
}
