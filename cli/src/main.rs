/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

mod config;
mod request;

use clap::{Parser, Subcommand, arg, CommandFactory};
use clap_complete::{generate, Shell};
use rpassword::read_password;
use std::io::Write;
use std::io;
use config::*;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: MainCommands,
}


#[derive(Subcommand, Debug)]
enum MainCommands {
    Config {
        key: String,
        value: Option<String>,
        #[arg(long, value_enum)]
        generate_completions: Option<Shell>,
    },
    Login {
        #[arg(short, long)]
        username: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        MainCommands::Config { key, value, generate_completions } => {
            if let Some(shell) = generate_completions {
                let mut app = Cli::command();
                let bin_name = app.get_name().to_string();
                generate(shell, &mut app, bin_name, &mut io::stdout());
                return;
            }

            set_get_value(key, value, false).unwrap_or(None);
        }

        MainCommands::Login { username } => {
            let username = if let Some(username) = username {
                username
            } else {
                print!("Username: ");
                std::io::stdout().flush().unwrap();
                let mut username = String::new();
                io::stdin().read_line(&mut username).expect("Failed to read username.");
                username.trim().to_string()
            };

            print!("Password: ");
            std::io::stdout().flush().unwrap();
            let password = read_password().unwrap();
        }
    }
}
