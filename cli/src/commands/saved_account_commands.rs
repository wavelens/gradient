/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::config::{
    LoginConfigFile, load_and_migrate_login_config_file, login_name_from_flag_env_or_config,
    update_login_config_file,
};
use crate::error::CliError;
use crate::input::{ask_for_choice, is_interactive};
use crate::output::{Color, ExitKind, Output};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use serde::Serialize;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List saved accounts
    List,
    /// Switch the account used by later commands. Without a name, pick one from the list
    Switch {
        /// Account name. Omit it to choose interactively
        #[arg(add = ArgValueCompleter::new(completion::complete_accounts))]
        name: Option<String>,
    },
    /// Delete a saved account and its login token
    Remove {
        /// Account name
        #[arg(add = ArgValueCompleter::new(completion::complete_accounts))]
        name: String,
    },
}

#[derive(Serialize)]
struct AccountRow {
    name: String,
    active: bool,
    server: Option<String>,
    username: Option<String>,
    logged_in: bool,
    selected_project: Option<String>,
}

fn rows(file: &LoginConfigFile) -> Vec<AccountRow> {
    let active = login_name_from_flag_env_or_config(file);
    file.accounts
        .iter()
        .map(|(name, login)| AccountRow {
            active: active.as_ref() == Some(name),
            name: name.clone(),
            server: login.server(),
            username: login.username.clone(),
            logged_in: login.token().is_some(),
            selected_project: login.selected_project.clone(),
        })
        .collect()
}

pub fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::List => {
            let rows = rows(&load_and_migrate_login_config_file());
            out.ok(&rows);
            if rows.is_empty() {
                out.human("No saved accounts. Run `gradient login <url>` to add one.");
            }
            let name_width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
            let server_width = rows
                .iter()
                .map(|r| r.server.as_deref().map_or(1, str::len))
                .max()
                .unwrap_or(0);
            for row in &rows {
                let marker = if row.active { "*" } else { " " };
                let who = match (&row.username, row.logged_in) {
                    (_, false) => out.paint("logged out", Color::Yellow),
                    (Some(user), true) => user.clone(),
                    (None, true) => "logged in".to_owned(),
                };
                let name = format!("{:name_width$}", row.name);
                let server = row.server.as_deref().unwrap_or("-");
                let project = row.selected_project.as_deref().unwrap_or("-");
                out.human(format!(
                    "{marker} {}  {server:server_width$}  {project}  {who}",
                    out.paint(&name, Color::Green)
                ));
            }
        }
        Commands::Switch { name } => {
            let name = name.unwrap_or_else(|| {
                let names: Vec<String> = load_and_migrate_login_config_file()
                    .accounts
                    .into_keys()
                    .collect();
                if names.is_empty() {
                    out.err(
                        ExitKind::Usage,
                        "No saved accounts. Run `gradient login <url>` to add one.",
                    );
                }
                if !is_interactive(out) {
                    out.fail(CliError::MissingArgument("NAME"));
                }
                ask_for_choice("Account", &names, out).unwrap_or_else(|| std::process::exit(0))
            });
            update_login_config_file(|file| {
                if !file.accounts.contains_key(&name) {
                    out.fail(CliError::AccountNotFound(name.clone()));
                }
                file.default_account = Some(name.clone());
            });
            out.ok(&serde_json::json!({ "default_account": name }));
            out.human(format!("Switched to account '{name}'."));
        }
        Commands::Remove { name } => {
            let removed_default = update_login_config_file(|file| {
                if file.accounts.remove(&name).is_none() {
                    out.fail(CliError::AccountNotFound(name.clone()));
                }
                let removed_default = file.default_account.as_deref() == Some(name.as_str());
                if removed_default {
                    file.default_account = None;
                }
                removed_default
            });
            out.ok(&serde_json::json!({ "removed": name, "default_cleared": removed_default }));
            if removed_default {
                out.human(format!(
                    "Deleted account `{name}`. Run `gradient account switch <name>` to choose the default account."
                ));
            } else {
                out.human(format!("Deleted account `{name}`."));
            }
        }
    }
}
