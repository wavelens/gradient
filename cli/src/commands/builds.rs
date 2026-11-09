/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::download::human_size;
use crate::commands::live_events::print_live_events_until_finished;
use crate::error::{CliError, FileAction};
use crate::input::client_from_config;
use crate::output::Output;
use clap::Subcommand;
use connector::DownloadedBinaryFile;
use connector::api::{BuildStatus, ClosureGraph};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show a build's dependency graph
    Graph {
        id: String,
        #[arg(short = 'i', long)]
        interactive: bool,
    },
    /// View a build's log
    Log {
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
    },
    /// Queue a failed, timed-out, aborted or dependency-failed build again, together with the builds that failed only because of it
    Retry { id: String },
    /// Abort a build of an active evaluation, whether created, queued, or running.
    Abort { id: String },
    /// Assign this build and its unresolved dependencies before other work. Only a superuser can run this command.
    Prioritize { id: String },
    /// Show the dependency closure of the build with the size of each store path.
    Closure {
        id: String,
        /// Show store paths for runtime use by the built output, not build-time dependencies.
        #[arg(long)]
        runtime_closure: bool,
    },
    /// Save the CycloneDX SBOM of the runtime closure of a build
    Sbom {
        id: String,
        /// Write the file to this path. Default: the server's proposed file name (in the current directory).
        #[arg(long)]
        output_file: Option<PathBuf>,
    },
    /// Print the live status and progress events of a build until the build finishes. Use `--json` to print one JSON line per event.
    Follow { id: String },
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
        } => {
            crate::commands::builds_log::handle_log(&id, interactive, lines, search, case, out)
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
        Commands::Sbom { id, output_file } => {
            let file = client
                .builds()
                .sbom(&id)
                .await
                .unwrap_or_else(|e| out.fail(e));
            save_downloaded_file(file, output_file, &format!("{id}.cdx.json"), out);
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

pub fn build_is_terminal(status: BuildStatus) -> bool {
    !matches!(
        status,
        BuildStatus::Created
            | BuildStatus::Queued
            | BuildStatus::Building
            | BuildStatus::FailedTransient
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
    out: Output,
) {
    let path = output_file.unwrap_or_else(|| {
        let suggested = file
            .suggested_filename
            .as_deref()
            .unwrap_or(fallback_filename);
        PathBuf::from(
            Path::new(suggested)
                .file_name()
                .unwrap_or(fallback_filename.as_ref()),
        )
    });
    std::fs::write(&path, &file.bytes)
        .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Write, &path, e)));
    let size = file.bytes.len();
    out.ok(&serde_json::json!({ "path": path, "bytes": size }));
    out.human(format!(
        "Saved {} ({})",
        path.display(),
        human_size(size as i64)
    ));
}
