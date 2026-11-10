/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::run_terminal::attach_terminal_to_job_serial_console;
use crate::error::{CliError, FileAction};
use crate::input::{OnOffSetting, client_from_config};
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use futures::StreamExt;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List the execution jobs of an evaluation
    List {
        /// Evaluation UUID
        evaluation: String,
    },
    /// Show details for an exec job
    Show {
        /// ID of the execution job
        job: String,
    },
    /// Print the logs for an exec job
    Logs {
        /// ID of the execution job
        job: String,
    },
    /// Cancel an exec job
    Cancel {
        /// ID of the execution job
        job: String,
    },
    /// List or download output files of an exec job
    Artifact {
        #[command(subcommand)]
        cmd: ExecJobArtifactCommand,
    },
    /// Keep the VM of an exec job running after the end of the program (`on`), or stop that VM (`off`).
    KeepVmRunning {
        /// ID of the execution job
        job: String,
        #[arg(value_enum)]
        setting: OnOffSetting,
    },
    /// Connect this terminal to the serial console of the VM for an exec job. Type `~.` at the start of a line to detach.
    Console {
        /// ID of the execution job
        job: String,
        /// Node index in a multi-node job. The first node is 0.
        #[arg(long)]
        node_index: Option<u32>,
    },
    /// List the pull requests that exec jobs open
    PullRequests {
        /// ID of the execution job
        job: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ExecJobArtifactCommand {
    /// List the output files that the exec job uploaded
    List {
        /// ID of the execution job
        job: String,
        /// Node index in a multi-node job. The first node is 0.
        #[arg(long)]
        node_index: Option<u32>,
    },
    /// Download one output file of an exec job
    Download {
        /// ID of the execution job
        job: String,
        /// Path of the output file visible in `artifact list`
        path: String,
        /// File to write to. Default: the name of the output file in the current directory.
        #[arg(long)]
        output_file_path: Option<PathBuf>,
        /// Node index in a multi-node job. The first node is 0.
        #[arg(long)]
        node_index: Option<u32>,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    let jobs = client.exec_jobs();
    match cmd {
        Commands::List { evaluation } => match jobs.of_evaluation(&evaluation).await {
            Ok(summaries) => {
                out.ok(&summaries);
                if summaries.is_empty() {
                    out.human("There are no exec jobs in this evaluation.");
                }
                for summary in &summaries {
                    out.human(format!(
                        "{} {} [{}]",
                        summary.id.0, summary.entry_point, summary.status
                    ));
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Show { job } => match jobs.get(&job).await {
            Ok(details) => {
                out.ok(&details);
                out.human(format!(
                    "{} {} [{}]",
                    details.id.0, details.entry_point, details.status
                ));
                if let Some(worker) = details.worker_name.as_ref().or(details.worker.as_ref()) {
                    out.human(format!("Worker: {worker}"));
                }
                if let Some(code) = details.program_exit_code {
                    out.human(format!("Exit code: {code}"));
                }
                if let Some(message) = &details.message {
                    out.human(message);
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Logs { job } => match jobs.log(&job, 0).await {
            Ok(exec_job_log_reply) => {
                out.ok(&serde_json::json!({"log": exec_job_log_reply.log}));
                out.human(exec_job_log_reply.log);
            }
            Err(e) => out.fail(e),
        },
        Commands::Cancel { job } => match jobs.abort(&job).await {
            Ok(_) => {
                out.ok(&serde_json::json!({"aborted": true}));
                out.human("Exec job canceled.");
            }
            Err(e) => out.fail(e),
        },
        Commands::Artifact {
            cmd: ExecJobArtifactCommand::List { job, node_index },
        } => match jobs.list_output_files(&job, node_index).await {
            Ok(files) => {
                out.ok(&files);
                if files.is_empty() {
                    out.human("No output file.");
                }
                for file in &files {
                    out.human(format!("{} ({} bytes)", file.path, file.size));
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Artifact {
            cmd:
                ExecJobArtifactCommand::Download {
                    job,
                    path,
                    output_file_path,
                    node_index,
                },
        } => {
            let destination = output_file_path
                .unwrap_or_else(|| PathBuf::from(path.rsplit('/').next().unwrap_or(path.as_str())));
            let mut chunks = jobs
                .download_job_output_file_from_node(&job, &path, node_index)
                .await
                .unwrap_or_else(|e| out.fail(e));
            let mut file = tokio::fs::File::create(&destination)
                .await
                .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Create, &destination, e)));
            let mut written: u64 = 0;
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.unwrap_or_else(|e| out.fail(e));
                file.write_all(&chunk).await.unwrap_or_else(|e| {
                    out.fail(CliError::file(FileAction::Write, &destination, e))
                });
                written += chunk.len() as u64;
            }
            file.flush()
                .await
                .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Write, &destination, e)));
            out.ok(&serde_json::json!({"path": destination, "bytes": written}));
            out.human(format!("Saved {}.", destination.display()));
        }
        Commands::KeepVmRunning { job, setting } => {
            let keep = setting == OnOffSetting::On;
            match jobs.set_vm_keep_running(&job, keep).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"keep_vm_running": keep}));
                    out.human(match keep {
                        true => "The VM can continue to run after program exit.",
                        false => "Program exits shut down the VM.",
                    });
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Console { job, node_index } => {
            if out.is_json() {
                out.err(
                    ExitKind::Usage,
                    "The console can run only with an interactive terminal. The `--json` flag does not work.",
                );
            }
            attach_terminal_to_job_serial_console(&client, &job, node_index, out).await
        }
        Commands::PullRequests { job } => match jobs.get_exec_job_pull_requests(&job).await {
            Ok(pull_requests) => {
                out.ok(&pull_requests);
                if pull_requests.is_empty() {
                    out.human("No pull requests exist.");
                }
                for pull_request in &pull_requests {
                    out.human(format!(
                        "#{} {} ({}): {}",
                        pull_request.number,
                        pull_request.branch,
                        pull_request.integration,
                        pull_request.url
                    ));
                }
            }
            Err(e) => out.fail(e),
        },
    }
}
