/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Parser, Subcommand, ValueEnum};
use gradient_daemon::{control, mock, server};
use std::path::PathBuf;

const CONTROL_SOCKET: &str = "/run/gradient-daemon/control.sock";

#[derive(Parser)]
#[command(name = "gradient-daemon", about = "Nix daemon protocol server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendKind {
    Mock,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, value_enum)]
        backend: BackendKind,
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value = "/nix/var/nix/daemon-socket/socket")]
        socket: PathBuf,
        #[arg(long, default_value = CONTROL_SOCKET)]
        control: PathBuf,
        #[arg(long, default_value = "/")]
        root: PathBuf,
        #[arg(long, default_value = "/nix/var/nix/db/db.sqlite")]
        base_db: PathBuf,
    },
    Ctl {
        cmd: String,
        #[arg(default_value = "{}")]
        args: String,
        #[arg(long, default_value = CONTROL_SOCKET)]
        control: PathBuf,
    },
    Mock {
        #[command(subcommand)]
        command: MockCommand,
    },
}

#[derive(Subcommand)]
enum MockCommand {
    CacheExport {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        secret_key_file: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

async fn serve_mock(
    spec: PathBuf,
    socket: PathBuf,
    control_path: PathBuf,
    root: PathBuf,
    base_db: PathBuf,
) -> anyhow::Result<()> {
    let config = mock::spec::DaemonConfig::load(&spec)?;
    let backend = mock::MockBackend::new(config, root, Some(&base_db)).await?;
    let control_backend = backend.clone();
    let control_task =
        tokio::spawn(async move { control::serve_control(control_backend, &control_path).await });
    tokio::select! {
        served = server::serve(backend, &socket) => served,
        controlled = control_task => controlled?,
    }
}

async fn ctl(cmd: String, args: String, path: PathBuf) -> anyhow::Result<()> {
    let reply = control::request(&path, &cmd, serde_json::from_str(&args)?).await?;
    println!("{}", serde_json::to_string(&reply["result"])?);
    anyhow::ensure!(reply["ok"] == true, "{}", reply["error"]);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Serve {
            backend: BackendKind::Mock,
            spec,
            socket,
            control,
            root,
            base_db,
        } => serve_mock(spec, socket, control, root, base_db).await,
        Command::Ctl { cmd, args, control } => ctl(cmd, args, control).await,
        Command::Mock {
            command:
                MockCommand::CacheExport {
                    spec,
                    secret_key_file,
                    out,
                },
        } => {
            let config = mock::spec::DaemonConfig::load(&spec)?;
            let key = std::fs::read_to_string(secret_key_file)?;
            let count = mock::cache_export::export(&config, &key, &out)?;
            eprintln!("exported {count} paths");
            Ok(())
        }
    }
}
