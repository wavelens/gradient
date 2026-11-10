/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::builds::{
    print_closure_size_graph, print_server_action_message_or_exit, save_downloaded_file,
};
use crate::commands::live_events::print_live_events_until_finished;
use crate::commands::poll_evaluation_until_finished::print_status_changes_and_exit;
use crate::input::{client_from_config, evaluation_uuid_from_reference};
use crate::output::Output;
use crate::tui::watch::eval_is_terminal;
use clap::Subcommand;
use connector::api::EvaluationFailureSummary;
use connector::evals::EvaluationReportQueryParameters;
use connector::task_evaluation_build_reference_parser::TaskOrEvaluationReference;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start a new evaluation of the same commit, build the failed builds again and print the evaluation UUID.
    Retry {
        evaluation: TaskOrEvaluationReference,
    },
    /// Abort the running evaluation
    Abort {
        evaluation: TaskOrEvaluationReference,
    },
    /// Assign all builds of the running evaluation ahead of other work. Superuser access only.
    Prioritize {
        evaluation: TaskOrEvaluationReference,
    },
    /// Show failed builds, attributes that failed to evaluate and packages fixed since the previous evaluation.
    Summary {
        evaluation: TaskOrEvaluationReference,
    },
    /// Save a SQLite diagnostic report of the evaluation so the file can go into a bug report. Default settings anonymize identifying information and include failed build logs.
    Report {
        evaluation: TaskOrEvaluationReference,
        /// Write the file to this path. Default: the server's proposed file name (in the current directory).
        #[arg(long)]
        output_file: Option<PathBuf>,
        /// Keep usernames, email addresses and repository URLs. Do not replace them with anonymous tokens.
        #[arg(long)]
        keep_real_identities: bool,
        /// Replace package names with anonymous tokens too.
        #[arg(long)]
        anonymize_package_names: bool,
        /// Omit logs of failed builds from the report.
        #[arg(long)]
        exclude_failed_build_logs: bool,
        /// Include server workers and upstream caches. Use only with permission to manage workers.
        #[arg(long)]
        include_worker_fleet: bool,
        /// Include programs that were run on exec workers for this evaluation.
        #[arg(long)]
        include_exec_jobs: bool,
    },
    /// Show the dependency closure for every entry point of an evaluation and the size of each store path.
    Closure {
        evaluation: TaskOrEvaluationReference,
        /// Show store paths for runtime use by the built output, not build-time dependencies.
        #[arg(long)]
        runtime_closure: bool,
    },
    /// Show the flake outputs that the evaluation scanned, with type and derivation path.
    ShowFlakeGraph {
        evaluation: TaskOrEvaluationReference,
    },
    /// Print live events for an evaluation until the evaluation is complete. Pass `--json` to print one line of JSON per event.
    Follow {
        evaluation: TaskOrEvaluationReference,
        /// Exit with the evaluation result. 0 on completion, 1 on failure, 4 on abort.
        #[arg(long)]
        exit_code: bool,
    },
    /// Wait for evaluation to complete. Print one line on each status change. Exit with 0 on completion, 1 on failure, 4 on abort and 5 after the time limit set by `--timeout`.
    Wait {
        evaluation: TaskOrEvaluationReference,
        /// Stop waiting after this number of seconds and exit with code 5.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    let evals = client.evals();
    match cmd {
        Commands::Retry { evaluation } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let id = evals
                .retry(&evaluation)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&id);
            out.human(format!("Started evaluation {id}"));
        }
        Commands::Abort { evaluation } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            print_server_action_message_or_exit(evals.abort(&evaluation).await, out)
        }
        Commands::Prioritize { evaluation } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            print_server_action_message_or_exit(evals.prioritize(&evaluation).await, out)
        }
        Commands::Summary { evaluation } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let summary = evals
                .summary(&evaluation)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&summary);
            print_evaluation_failure_summary(&summary, out);
        }
        Commands::Report {
            evaluation,
            output_file,
            keep_real_identities,
            anonymize_package_names,
            exclude_failed_build_logs,
            include_worker_fleet,
            include_exec_jobs,
        } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let parameters = EvaluationReportQueryParameters {
                anonymize_identities: !keep_real_identities,
                anonymize_packages: anonymize_package_names,
                include_logs: !exclude_failed_build_logs,
                include_instance: include_worker_fleet,
                include_exec: include_exec_jobs,
            };
            let file = evals
                .report(&evaluation, &parameters)
                .await
                .unwrap_or_else(|e| out.fail(e));
            save_downloaded_file(file, output_file, &format!("{evaluation}.db"), out);
        }
        Commands::Closure {
            evaluation,
            runtime_closure,
        } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let graph = if runtime_closure {
                evals.runtime_closure(&evaluation).await
            } else {
                evals.closure(&evaluation).await
            };
            print_closure_size_graph(&graph.unwrap_or_else(|e| out.fail(e)), out);
        }
        Commands::ShowFlakeGraph { evaluation } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let nodes = evals
                .flake_graph(&evaluation)
                .await
                .unwrap_or_else(|e| out.fail(e));
            out.ok(&nodes);
            for node in &nodes {
                let derivation = node.drv_path.as_deref().unwrap_or("");
                out.human(format!("{}  {}  {derivation}", node.path, node.kind));
            }
        }
        Commands::Follow {
            evaluation,
            exit_code,
        } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            let events = evals
                .live(&evaluation)
                .await
                .unwrap_or_else(|e| out.fail(e));
            print_live_events_until_finished(
                events,
                || async { Ok(eval_is_terminal(evals.get(&evaluation).await?.status)) },
                out,
            )
            .await;
            if exit_code {
                print_status_changes_and_exit(&client, &evaluation, None, out).await;
            }
        }
        Commands::Wait {
            evaluation,
            timeout,
        } => {
            let evaluation = evaluation_uuid_from_reference(&client, &evaluation, out).await;
            print_status_changes_and_exit(
                &client,
                &evaluation,
                timeout.map(Duration::from_secs),
                out,
            )
            .await;
        }
    }
}

fn print_evaluation_failure_summary(summary: &EvaluationFailureSummary, out: Output) {
    if out.is_json() {
        return;
    }
    match &summary.compared_with {
        Some(compared) => out.human(format!(
            "Comparison with Evaluation {} for Commit {}",
            compared.id, compared.commit
        )),
        None => out.human("No previous evaluation to compare."),
    }
    let packages = &summary.packages;
    out.human(format!(
        "Packages: {} total, {} built, {} failed, {} blocked, {} unfinished",
        packages.total, packages.built, packages.failed, packages.blocked, packages.unfinished
    ));
    let newly_failing_suffix = |newly_failing: bool| if newly_failing { "  New" } else { "" };
    if !summary.failures.is_empty() {
        out.human(format!("\nFailed Builds ({})", summary.failures_total));
        for failure in &summary.failures {
            out.human(format!(
                "  {}  {}  {}  Blocked packages: {}{}",
                failure.name,
                failure.architecture,
                failure.status,
                failure.blocked_total,
                newly_failing_suffix(failure.newly_failing)
            ));
        }
    }
    if !summary.failed_attributes.is_empty() {
        out.human(format!(
            "\nFailed Attributes ({})",
            summary.failed_attributes.len()
        ));
        for attribute in &summary.failed_attributes {
            out.human(format!(
                "  {}{}: {}",
                attribute.attr,
                newly_failing_suffix(attribute.newly_failing),
                attribute.message
            ));
        }
    }
    if !summary.fixed.is_empty() {
        out.human(format!("\nFixed Packages ({})", summary.fixed_total));
        for package in &summary.fixed {
            out.human(format!("  {package}"));
        }
    }
}
