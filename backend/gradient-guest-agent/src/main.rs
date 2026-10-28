/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod channels;
mod cli_config;
mod context;
mod http;
mod listener;
mod output;
mod phase;
mod redaction;
mod run;
mod setup;
mod shell;
mod terminal;
mod tree_patch;
mod upload;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use gradient_util::shutdown::Shutdown;

use http::HttpState;
use output::OutputLog;

const OUTPUT_FILE: &str = "/run/gradient/output.log";
const OUTPUT_KEPT: u64 = 64 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "gradient-guest-agent", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Phase { name: String, file: Option<PathBuf> },
    Upload { file: PathBuf, path: Option<String> },
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        None => serve().await.map(|()| ExitCode::SUCCESS),
        Some(Command::Phase { name, file }) => {
            post(&http::phase_route(&name), file.as_deref()).await
        }
        Some(Command::Upload { file, path }) => {
            let path = path.unwrap_or_else(|| file_name(&file));
            post(&http::output_route(&path), Some(&file)).await
        }
    }
}

async fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let restarted = tokio::fs::try_exists(OUTPUT_FILE).await?;
    let output = OutputLog::create(Path::new(OUTPUT_FILE), OUTPUT_KEPT).await?;
    let readers = output.readers();
    let state = Arc::new(HttpState::new(output));
    let tasks = Shutdown::new();
    tokio::try_join!(
        listener::listen(Arc::clone(&state), readers, tasks, restarted),
        http::serve(state)
    )?;
    Ok(())
}

async fn post(route: &str, file: Option<&Path>) -> Result<ExitCode> {
    let answer = http::post_file(route, file).await?;
    if answer.ok {
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("{}", answer.body);
    Ok(ExitCode::FAILURE)
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
