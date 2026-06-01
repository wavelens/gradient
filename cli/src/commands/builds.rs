/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show a build's dependency graph
    Graph {
        id: String,
        #[arg(short = 'i', long)]
        interactive: bool,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Graph { id, interactive } => {
            let client = client_from_config(out);
            match client.builds().graph(&id).await {
                Ok(g) => {
                    if interactive && !out.is_json() {
                        crate::tui::run(crate::tui::graph::GraphTree::from_build_graph(&g))
                            .unwrap_or_else(|e| out.err(ExitKind::Api, format!("tui error: {e}")));
                    } else {
                        out.ok(&g);
                        out.human(format!("{} nodes, {} edges", g.nodes.len(), g.edges.len()));
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
    }
}
