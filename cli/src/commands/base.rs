/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use crate::config::*;
use crate::input::*;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use connector::auth::{MakeLoginRequest, MakeUserRequest};
use std::io;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
#[command(arg_required_else_help = true, subcommand_required = true)]
struct Cli {
    /// Emit machine-readable JSON envelopes; disables interactive prompts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: MainCommands,
}

#[derive(Subcommand, Debug)]
enum MainCommands {
    /// Generate shell completions
    Completion {
        /// The shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Get or set configuration values
    Config { key: String, value: Option<String> },
    /// Check server connection status
    Status,
    /// Register a new user account
    Register {
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        email: Option<String>,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Login to the server
    Login {
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Logout from the server
    Logout,
    /// Display current user information
    Info,
    /// Manage organizations
    Organization {
        #[command(subcommand)]
        cmd: organization::Commands,
    },
    /// Manage projects
    Project {
        #[command(subcommand)]
        cmd: project::Commands,
    },
    /// Manage build workers
    Worker {
        #[command(subcommand)]
        cmd: worker::Commands,
    },
    /// Manage build caches
    Cache {
        #[command(subcommand)]
        cmd: cache::Commands,
    },
    /// Submit a build request from the current git repository
    Build {
        /// Eval target attribute path (default: project's wildcard)
        target: Option<String>,
        /// Target system (default: organization preference)
        #[arg(long)]
        system: Option<String>,
        #[arg(short, long)]
        organization: Option<String>,
        /// Skip log streaming and exit after dispatch
        #[arg(long)]
        no_stream: bool,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Download evaluation artefacts
    Download {
        /// Flake-output attribute spec, e.g. '#packages.x86_64-linux.my-app'. Comma-separated for multiple.
        flake_ref: Option<String>,
        /// Skip the eval picker; use this evaluation directly
        #[arg(long)]
        evaluation: Option<String>,
        /// Restrict latest-eval lookup to a project (accepts `name` or `org/name`)
        #[arg(long)]
        project: Option<String>,
        /// Skip the product picker; comma-separated 1-based indices, ranges (`1-3`), or `all`
        #[arg(long, conflicts_with = "flake_ref")]
        products: Option<String>,
        /// Write to this directory (default: current directory)
        #[arg(long)]
        out: Option<String>,
    },
    /// Generate project files
    Generate {
        #[command(subcommand)]
        cmd: generate::Commands,
    },
    /// Hash a password as an argon2id PHC string for use in
    /// `services.gradient.state.users.<name>.password_file`.
    Hash,
}

pub async fn run_cli() -> std::io::Result<()> {
    let cli = Cli::parse();
    let out = Output::new(cli.json);

    match cli.cmd {
        MainCommands::Completion { shell } => {
            let mut app = Cli::command();
            let bin_name = app.get_name().to_string();
            generate(shell, &mut app, bin_name, &mut io::stdout());
        }
        MainCommands::Config { key, value } => {
            set_get_value_from_string(key, value, false)
                .map_err(|_| {
                    std::process::exit(1);
                })
                .unwrap();
        }

        MainCommands::Status => {
            let client = client_from_config(out);
            match client.health().await {
                Ok(msg) => {
                    out.ok(&serde_json::json!({"status": "online", "server": msg}));
                    out.human("Server Online.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        MainCommands::Register {
            username,
            name,
            email,
            password,
        } => {
            if out.is_json() && password.is_none() {
                out.err(ExitKind::Usage, "missing argument: --password");
            }

            let server_url = set_get_value(ConfigKey::Server, None, true);
            if server_url.is_none() {
                out.err(
                    ExitKind::Usage,
                    "Server URL not set. Use `gradient config server <url>`.",
                );
            }

            let input_fields = [("Username", username), ("Name", name), ("Email", email)]
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            let input = handle_input(input_fields, true);

            let pw = password.unwrap_or_else(ask_for_password);

            let client = client_from_config(out);
            match client
                .auth()
                .register(MakeUserRequest {
                    username: input.get("Username").unwrap().clone(),
                    name: input.get("Name").unwrap().clone(),
                    email: input.get("Email").unwrap().clone(),
                    password: pw,
                })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"registered": true}));
                    out.human("Registration successful. Please log in.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        MainCommands::Login { username, password } => {
            if out.is_json() && password.is_none() {
                out.err(ExitKind::Usage, "missing argument: --password");
            }

            let server_url = set_get_value(ConfigKey::Server, None, true);
            if server_url.is_none() {
                set_get_value(ConfigKey::Server, Some(ask_for_input("Server URL")), true).unwrap();
            }

            let username = username.unwrap_or_else(|| ask_for_input("Username"));
            let pw = password.unwrap_or_else(ask_for_password);

            let client = client_from_config(out);
            match client
                .auth()
                .basic_login(MakeLoginRequest {
                    loginname: username,
                    password: pw,
                })
                .await
            {
                Ok(token) => {
                    set_get_value(ConfigKey::AuthToken, Some(token), true).unwrap();
                    out.ok(&serde_json::json!({"logged_in": true}));
                    out.human("Logged in.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        MainCommands::Info => {
            let client = client_from_config(out);
            match client.user().get().await {
                Ok(user) => {
                    out.ok(&user);
                    out.human(format!("User ID: {}", user.id));
                    out.human(format!("Username: {}", user.username));
                    out.human(format!("Name: {}", user.name));
                    out.human(format!("Email: {}", user.email));
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        MainCommands::Logout => {
            set_get_value(ConfigKey::AuthToken, Some(String::new()), true).unwrap();
            out.ok(&serde_json::json!({"logged_out": true}));
            out.human("Logged out.");
        }

        MainCommands::Build {
            target,
            system,
            organization,
            no_stream,
            quiet,
        } => build::handle_build(target, system, organization, no_stream, quiet, out).await,
        MainCommands::Download {
            flake_ref,
            evaluation,
            project,
            products,
            out: out_dir,
        } => {
            download::handle_download(flake_ref, evaluation, project, products, out_dir, out).await
        }
        MainCommands::Organization { cmd } => organization::handle(cmd, out).await,
        MainCommands::Project { cmd } => project::handle(cmd, out).await,
        MainCommands::Worker { cmd } => worker::handle(cmd, out).await,
        MainCommands::Cache { cmd } => cache::handle(cmd, out).await,
        MainCommands::Generate { cmd } => generate::handle(cmd, out).await,
        MainCommands::Hash => {
            let password = ask_for_password();
            let confirm = ask_for_password();
            if password != confirm {
                out.err(ExitKind::Usage, "Passwords did not match.");
            }
            println!("{}", password_auth::generate_hash(password.as_bytes()));
        }
    }

    std::process::exit(0);
}
