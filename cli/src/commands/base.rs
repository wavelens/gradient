/*
 * spdx-filecopyrighttext: 2025 wavelens ug <info@wavelens.io>
 *
 * spdx-license-identifier: agpl-3.0-only
 */

use super::*;
use crate::config::*;
use crate::input::*;
use clap::{CommandFactory, Parser, Subcommand, arg};
use clap_complete::{Shell, generate};
use connector::*;
use std::io;
use std::process::exit;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<MainCommands>,
    #[arg(long, value_enum)]
    generate_completions: Option<Shell>,
}

// TODO: display help when no subcommand is given
// TODO: check selected organization and project before running commands
#[derive(Subcommand, Debug)]
enum MainCommands {
    Config {
        key: String,
        value: Option<String>,
    },
    Status,
    Register {
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        email: Option<String>,
    },
    Login {
        #[arg(short, long)]
        username: Option<String>,
    },
    Logout,
    Info,
    Organization {
        #[command(subcommand)]
        cmd: organization::Commands,
    },
    Project {
        #[command(subcommand)]
        cmd: project::Commands,
    },
    Server {
        #[command(subcommand)]
        cmd: server::Commands,
    },
    Cache {
        #[command(subcommand)]
        cmd: cache::Commands,
    },
    Build {
        derivation: String,
        #[arg(short, long)]
        organization: Option<String>,
        #[arg(short, long)]
        quiet: bool,
    },
    Download {
        #[arg(short, long)]
        build_id: Option<String>,
        #[arg(short, long)]
        filename: Option<String>,
    },
}

pub async fn run_cli() -> std::io::Result<()> {
    let cli = Cli::parse();

    if let Some(shell) = cli.generate_completions {
        let mut app = Cli::command();
        let bin_name = app.get_name().to_string();
        generate(shell, &mut app, bin_name, &mut io::stdout());
        return Ok(());
    }

    if let Some(cmd) = cli.cmd {
        match cmd {
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
                    eprintln!(
                        "Server URL is not set. Use `gradient config server <url>` to set it."
                    );
                    std::process::exit(1);
                }

                if auth_token.is_none() {
                    eprintln!("Not logged in. Use `gradient login` to log in.");
                    std::process::exit(1);
                }

                health(get_request_config(config).unwrap())
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                println!("Server Online.");
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

                println!("Registration successful. Please log in.");
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

                println!("User ID: {}", res.message.id);
                println!("Username: {}", res.message.username);
                println!("Name: {}", res.message.name);
                println!("Email: {}", res.message.email);
            }

            MainCommands::Logout => {
                set_get_value(ConfigKey::AuthToken, Some(String::new()), true).unwrap();
                println!("Logged out.");
            }

            MainCommands::Build {
                derivation,
                organization,
                quiet,
            } => build::handle_build(derivation, organization, quiet).await,
            MainCommands::Download { build_id, filename } => {
                download::handle_download(build_id, filename).await
            }
            MainCommands::Organization { cmd } => organization::handle(cmd).await,
            MainCommands::Project { cmd } => project::handle(cmd).await,
            MainCommands::Server { cmd } => server::handle(cmd).await,
            MainCommands::Cache { cmd } => cache::handle(cmd).await,
        }
    } else {
        eprintln!("No subcommand provided");
        exit(1);
    }

    exit(0);
}
