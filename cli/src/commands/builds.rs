/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::download::human_size;
use crate::commands::live_events::print_live_events_until_finished;
use crate::commands::mcp_command_run_decisions::{
    McpToolCommandPermission::McpToolMayRunCommand, NOT_AVAILABLE_BECAUSE_COMMAND_USES_LOCAL_FILES,
};
use crate::download_file_path_and_write::{
    DownloadDestination, checked_file_path_in_directory_from_server_file_name,
    write_bytes_to_download_destination,
};
use crate::error::CliError;
use crate::input::{client_from_config, parse_build_uuid_or_web_ui_log_page_url};
use crate::output::Output;
use clap::Subcommand;
use connector::DownloadedBinaryFile;
use connector::api::{BuildStatus, ClosureGraph};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show a build's dependency graph
    #[command(add = McpToolMayRunCommand)]
    Graph {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
        #[arg(short = 'i', long)]
        interactive: bool,
    },
    /// View a build's log
    #[command(add = McpToolMayRunCommand)]
    Log {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Print only a line range, e.g. `L120-L130`, `120-130`, or `120`.
        #[arg(long)]
        lines: Option<String>,
        /// Search the log for a substring and stream matching lines.
        #[arg(long)]
        search: Option<String>,
        /// Make `--search` case-sensitive.
        #[arg(long)]
        case: bool,
        /// Read the log for this attempt instead of the latest attempt. Attempts start at 1 and count up in chronological order.
        #[arg(long)]
        attempt: Option<u32>,
    },
    /// Re-queue builds in a running evaluation that failed, timed out, were canceled, or failed because of a dependency. Also re-queue builds that failed only because of those builds. Use `gradient evaluation retry` to start a new evaluation when an evaluation is complete.
    #[command(add = McpToolMayRunCommand)]
    Retry {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
    },
    /// Abort a build of an active evaluation, whether created, queued, or running.
    #[command(add = McpToolMayRunCommand)]
    Abort {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
    },
    /// Assign this build and its unresolved dependencies before other work. Only a superuser can run this command.
    #[command(add = McpToolMayRunCommand)]
    Prioritize {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
    },
    /// Show the dependency closure of the build with the size of each store path.
    #[command(add = McpToolMayRunCommand)]
    Closure {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
        /// Show store paths for runtime use by the built output, not build-time dependencies.
        #[arg(long)]
        runtime_closure: bool,
    },
    /// Save the CycloneDX SBOM of the runtime closure of a build
    #[command(add = NOT_AVAILABLE_BECAUSE_COMMAND_USES_LOCAL_FILES)]
    Sbom {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
        /// Write the file to this path. Default: the server's proposed file name (in the current directory).
        #[arg(long)]
        output_file: Option<PathBuf>,
        /// Replace an existing file at the server-proposed filename
        #[arg(long)]
        replace_existing: bool,
    },
    /// Print the live status and progress events of a build until the build finishes. Use `--json` to print one JSON line per event.
    #[command(add = McpToolMayRunCommand)]
    Follow {
        #[arg(value_parser = parse_build_uuid_or_web_ui_log_page_url)]
        id: String,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    match cmd {
        Commands::Graph { id, interactive } => match client.builds().graph(&id).await {
            Ok(g) => {
                if interactive && !out.is_json() {
                    crate::tui::run(crate::tui::graph::GraphTree::from_build_graph(&g))
                        .unwrap_or_else(|e| out.fail(CliError::TerminalUi(e)));
                } else {
                    out.ok(&g);
                    out.human(format!("{} nodes, {} edges", g.nodes.len(), g.edges.len()));
                }
            }
            Err(e) => out.fail(e),
        },
        Commands::Log {
            id,
            interactive,
            lines,
            search,
            case,
            attempt,
        } => {
            crate::commands::builds_log::handle_log(
                &id,
                crate::commands::builds_log::BuildsLogCommandFlags {
                    interactive,
                    lines,
                    search,
                    case,
                    attempt,
                },
                out,
            )
            .await
        }
        Commands::Retry { id } => {
            print_server_action_message_or_exit(client.builds().retry(&id).await, out)
        }
        Commands::Abort { id } => {
            print_server_action_message_or_exit(client.builds().abort(&id).await, out)
        }
        Commands::Prioritize { id } => {
            print_server_action_message_or_exit(client.builds().prioritize(&id).await, out)
        }
        Commands::Closure {
            id,
            runtime_closure,
        } => {
            let graph = if runtime_closure {
                client.builds().runtime_closure(&id).await
            } else {
                client.builds().closure(&id).await
            };
            print_closure_size_graph(&graph.unwrap_or_else(|e| out.fail(e)), out);
        }
        Commands::Sbom {
            id,
            output_file,
            replace_existing,
        } => {
            let file = client
                .builds()
                .sbom(&id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            save_downloaded_file(
                file,
                output_file,
                &format!("{id}.cdx.json"),
                replace_existing,
                out,
            );
        }
        Commands::Follow { id } => {
            let events = client
                .builds()
                .live(&id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            print_live_events_until_finished(
                events,
                || async { Ok(build_is_terminal(client.builds().get(&id).await?.status)) },
                out,
            )
            .await
        }
    }
}

pub fn print_server_action_message_or_exit(
    result: Result<String, connector::ConnectorError>,
    out: Output,
) {
    let message = result.unwrap_or_else(|e| out.fail(e));
    out.ok(&message);
    out.human(message);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildClass {
    Succeeded,
    Failed,
    Building,
    Queued,
    AbortedSkippedOrUnknown,
}

impl BuildClass {
    pub fn build_class_from_build_status(status: BuildStatus) -> Self {
        match status {
            BuildStatus::Completed | BuildStatus::Substituted => Self::Succeeded,
            BuildStatus::Building => Self::Building,
            BuildStatus::Created | BuildStatus::Queued | BuildStatus::FailedTransient => {
                Self::Queued
            }
            BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::DependencyFailed => Self::Failed,
            BuildStatus::Aborted | BuildStatus::Skipped => Self::AbortedSkippedOrUnknown,
        }
    }

    pub fn build_class_from_status_text(status: &str) -> Self {
        status.parse().map_or(
            Self::AbortedSkippedOrUnknown,
            Self::build_class_from_build_status,
        )
    }
}

pub fn build_is_terminal(status: BuildStatus) -> bool {
    !matches!(
        BuildClass::build_class_from_build_status(status),
        BuildClass::Building | BuildClass::Queued
    )
}

pub fn print_closure_size_graph(graph: &ClosureGraph, out: Output) {
    out.ok(graph);
    if out.is_json() {
        return;
    }
    let total = graph
        .total_size_bytes
        .map_or_else(|| "?".to_owned(), human_size);
    out.human(format!(
        "{} store paths, {} references, total {}",
        graph.node_count, graph.edge_count, total
    ));
    let mut nodes: Vec<_> = graph.nodes.iter().collect();
    nodes.sort_by_key(|node| std::cmp::Reverse(node.nar_size.unwrap_or(0)));
    for node in nodes {
        let size = node.nar_size.map_or_else(|| "?".to_owned(), human_size);
        out.human(format!("{size:>10}  {}", node.path));
    }
    if graph.truncated {
        out.human("Only the heaviest store paths appear in the list.");
    }
}

pub fn save_downloaded_file(
    file: DownloadedBinaryFile,
    output_file: Option<PathBuf>,
    fallback_filename: &str,
    replace_existing: bool,
    out: Output,
) {
    let destination = match output_file {
        Some(path) => DownloadDestination::CommandLinePath(path),
        None => {
            let current_directory = Path::new("");
            let path = file
                .suggested_filename
                .as_deref()
                .and_then(|suggested| {
                    checked_file_path_in_directory_from_server_file_name(
                        current_directory,
                        suggested,
                    )
                    .ok()
                })
                .unwrap_or_else(|| current_directory.join(fallback_filename));
            DownloadDestination::FileInServerNamedDirectory {
                path,
                replace_existing,
            }
        }
    };
    write_bytes_to_download_destination(&file.bytes, &destination).unwrap_or_else(|e| out.fail(e));
    let path = destination.path();
    let size = file.bytes.len();
    out.ok(&serde_json::json!({ "path": path, "bytes": size }));
    out.human(format!(
        "Saved {} ({})",
        path.display(),
        human_size(size as i64)
    ));
}
