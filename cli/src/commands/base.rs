/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use crate::config::*;
use crate::input::*;
use crate::output::{ExitKind, Output};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use connector::*;
use std::io;
use std::process::exit;

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
    },
    /// Login to the server
    Login {
        #[arg(short, long)]
        username: Option<String>,
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
        /// Skip the eval picker; use this evaluation directly
        #[arg(long)]
        evaluation: Option<String>,
        /// Restrict latest-eval lookup to a project (accepts `name` or `org/name`)
        #[arg(long)]
        project: Option<String>,
        /// Skip the product picker; comma-separated 1-based indices, ranges (`1-3`), or `all`
        #[arg(long)]
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
                    exit(1);
                })
                .unwrap();
        }

        MainCommands::Status => {
            let config = load_config();
            let server_url = set_get_value(ConfigKey::Server, None, true);
            let auth_token = set_get_value(ConfigKey::AuthToken, None, true);

            if server_url.is_none() {
                out.err(
                    ExitKind::Usage,
                    "Server URL is not set. Use `gradient config server <url>` to set it.",
                );
            }

            if auth_token.is_none() {
                out.err(ExitKind::Unauthorized, "Not logged in. Use `gradient login` to log in.");
            }

            health(get_request_config(config).unwrap())
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

            out.human("Server Online.");
        }

        MainCommands::Register {
            username,
            name,
            email,
        } => {
            let server_url = set_get_value(ConfigKey::Server, None, true);

            if server_url.is_none() {};

            let input_fields = [("Username", username), ("Name", name), ("Email", email)]
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();

            let input = handle_input(input_fields, true);

            let password = ask_for_password();

            let res = auth::post_basic_register(
                get_request_config(load_config()).unwrap(),
                input.get("Username").unwrap().clone(),
                input.get("Name").unwrap().clone(),
                input.get("Email").unwrap().clone(),
                password,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Registration failed: {}", res.message);
                exit(1);
            }

            out.human("Registration successful. Please log in.");
        }

        MainCommands::Login { username } => {
            let server_url = set_get_value(ConfigKey::Server, None, true);

            if server_url.is_none() {
                set_get_value(ConfigKey::Server, Some(ask_for_input("Server URL: ")), true)
                    .unwrap();
            };

            let username = if let Some(username) = username {
                username
            } else {
                ask_for_input("Username")
            };

            let password = ask_for_password();

            let res = auth::post_basic_login(
                get_request_config(load_config()).unwrap(),
                username,
                password,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Login failed: {}", res.message);
                exit(1);
            }

            set_get_value(ConfigKey::AuthToken, Some(res.message), true).unwrap();
        }

        MainCommands::Info => {
            let res = user::get(get_request_config(load_config()).unwrap())
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

            if res.error {
                eprintln!("Failed to get user info.");
                exit(1);
            }

            out.human(format!("User ID: {}", res.message.id));
            out.human(format!("Username: {}", res.message.username));
            out.human(format!("Name: {}", res.message.name));
            out.human(format!("Email: {}", res.message.email));
        }

        MainCommands::Logout => {
            set_get_value(ConfigKey::AuthToken, Some(String::new()), true).unwrap();
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
            evaluation,
            project,
            products,
            out: out_dir,
        } => download::handle_download(evaluation, project, products, out_dir, out).await,
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

    exit(0);
}
