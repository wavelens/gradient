/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use crate::config::*;
use crate::error::CliError;
use crate::input::*;
use crate::output::{ExitKind, Output};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use clap_complete::{CompleteEnv, Shell};
use connector::Client;
use connector::api::{CliDevicePollRequest, MakeLoginRequest, MakeUserRequest};
use connector::auth::CliPollOutcome;
use std::io::{self, Write};
use std::process::Command;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
#[command(arg_required_else_help = true, subcommand_required = true)]
struct Cli {
    /// Emit machine-readable JSON envelopes without interactive prompts.
    #[arg(long, global = true)]
    json: bool,
    /// The account saved to run this command. Default: the account selected by `gradient account switch`.
    #[arg(long = "account", global = true, env = "GRADIENT_ACCOUNT", value_name = "NAME", add = ArgValueCompleter::new(completion::complete_accounts))]
    account_name: Option<String>,
    /// The project to run this command in, instead of the selected project. This flag doesn't save anything.
    #[arg(long = "project", global = true, value_name = "NAME", add = ArgValueCompleter::new(completion::complete_projects))]
    project_name: Option<String>,
    /// Specify the task for this command as NAME or PROJECT/NAME instead of the selected task. No changes persist.
    #[arg(long = "task", global = true, value_name = "TASK", add = ArgValueCompleter::new(completion::complete_tasks))]
    task_override: Option<String>,
    #[command(subcommand)]
    cmd: MainCommands,
}

#[derive(Subcommand, Debug)]
enum MainCommands {
    /// Manage saved accounts. Each account is a server. The server has its own login, selected project and selected task.
    Account {
        #[command(subcommand)]
        cmd: saved_account_commands::Commands,
    },
    /// Generate shell completions
    #[command(hide = true)]
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
        /// Server URL to log in to. It is stored as the configured server, replacing a separate
        /// `gradient config server`.
        server: Option<String>,
        /// Use basic username/password instead of the default web flow
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        password: Option<String>,
        /// Skip opening the browser and print the URL instead
        #[arg(long)]
        no_browser: bool,
    },
    /// Logout: revoke the login token on the server, then remove it from this machine
    Logout,
    /// Display current user information
    Info,
    /// Manage SSH keys, API keys, sessions, invitations, linked logins and settings for the logged-in account.
    LoggedInUser {
        #[command(subcommand)]
        cmd: logged_in_user_account_group_commands::Commands,
    },
    /// Manage projects
    Project {
        #[command(subcommand)]
        cmd: project::Commands,
    },
    /// Manage tasks
    Task {
        #[command(subcommand)]
        cmd: task::Commands,
    },
    /// Open pull requests from the working tree
    Pr {
        #[command(subcommand)]
        cmd: pr::Commands,
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
    /// Manage a team and its members, invitations, grants and workers.
    Team {
        #[command(subcommand)]
        cmd: team::Commands,
    },
    /// Submit a build request from the current git repository
    Build {
        /// Eval target attribute path (default: task's wildcard)
        target: Option<String>,
        /// Target system (default: project preference)
        #[arg(long)]
        system: Option<String>,
        /// Start the build and return the evaluation UUID without streaming logs
        #[arg(short, long)]
        background: bool,
        #[arg(short, long)]
        quiet: bool,
        /// Do not produce a `result` symlink/folder after the build
        #[arg(long)]
        no_link: bool,
        /// Override a flake input, like `nix build`. Repeatable. REF must be a remote flake ref
        /// (github:, git+ssh://, flake:, ...). Local paths are not supported.
        #[arg(long = "override-input", num_args = 2, value_names = ["INPUT", "FLAKE"], action = clap::ArgAction::Append)]
        override_input: Vec<String>,
    },
    /// Run the flake program on an exec worker and stream the program output to this terminal
    Run(run::RunArgs),
    /// Inspect exec jobs and control them. An exec job is a program that `gradient run` can launch on an exec worker.
    ExecJob {
        #[command(subcommand)]
        cmd: exec_job_commands::Commands,
    },
    /// Manage the whole instance. Only a superuser account can run these commands.
    Administration {
        #[command(subcommand)]
        cmd: admin::Commands,
    },
    /// Watch a running evaluation's live build logs and status
    Watch {
        /// Evaluation UUID to watch
        evaluation: String,
    },
    /// Print the full logs of every build in an evaluation
    Logs {
        /// Evaluation UUID
        evaluation: String,
    },
    /// Download evaluation artefacts
    Download {
        /// Flake-output attribute spec, e.g. '#packages.x86_64-linux.my-app'. Comma-separated for multiple.
        flake_ref: Option<String>,
        /// Skip the eval picker and use this evaluation directly
        #[arg(long)]
        evaluation: Option<String>,
        /// Skip the product picker with comma-separated 1-based indices, ranges (`1-3`), or `all`
        #[arg(long, conflicts_with = "flake_ref")]
        products: Option<String>,
        /// Write to this directory (default: current directory)
        #[arg(long)]
        out: Option<String>,
    },
    /// Search projects, tasks, cache, commit entries and store paths. List starred items when the query is empty.
    Search {
        /// Text, a prefix of a commit hash, or a store path
        query: Option<String>,
        /// Maximum name matches per kind
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Inspect builds (dependency graph)
    Builds {
        #[command(subcommand)]
        cmd: builds::Commands,
    },
    /// Control and inspect evaluations. Retries, cancellation, prioritization, failure summaries, diagnostic reports, closures and live events.
    Evaluation {
        #[command(subcommand)]
        cmd: evaluation::Commands,
    },
    /// Generate configuration files
    Generate {
        #[command(subcommand)]
        cmd: generate::Commands,
    },
    /// Evaluate a flake's outputs to derivations, like nix-eval-jobs
    #[cfg(feature = "eval")]
    Eval(eval::EvalArgs),
    /// Serve this Gradient instance to MCP clients over stdio
    Mcp {
        /// Also expose tools that start, abort and watch evaluations
        #[arg(long)]
        control: bool,
    },
    /// Hash a password as an argon2id PHC string for use in
    /// `services.gradient.state.users.<name>.password_file`.
    Hash,
}

fn read_nonempty_environment_variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn store_invocation_overrides_from_cli_and_env(
    account: Option<String>,
    project: Option<String>,
    task: Option<String>,
) {
    set_invocation_overrides(InvocationOverrides {
        account,
        server: read_nonempty_environment_variable("GRADIENT_SERVER"),
        token: read_nonempty_environment_variable("GRADIENT_TOKEN"),
        project,
        task,
    });
}

pub fn parse_subcommand_name_chain_from_cli_args(args: &[String]) -> Option<Vec<String>> {
    let matches = Cli::command()
        .try_get_matches_from(std::iter::once("gradient").chain(args.iter().map(String::as_str)))
        .ok()?;
    let mut chain = Vec::new();
    let mut current = &matches;
    while let Some((name, sub)) = current.subcommand() {
        chain.push(name.to_owned());
        current = sub;
    }
    Some(chain)
}

pub fn complete_env() {
    if std::env::var_os("COMPLETE").is_some() {
        let matches = Cli::command()
            .ignore_errors(true)
            .try_get_matches_from(std::env::args().skip_while(|arg| arg != "--").skip(1))
            .ok();
        let non_empty_user_typed_value_for_argument_id = |id: &str| {
            matches
                .as_ref()
                .and_then(|m| m.get_one::<String>(id).cloned())
                .filter(|value| !value.is_empty())
        };
        store_invocation_overrides_from_cli_and_env(
            non_empty_user_typed_value_for_argument_id("account_name")
                .or_else(|| read_nonempty_environment_variable("GRADIENT_ACCOUNT")),
            non_empty_user_typed_value_for_argument_id("project_name"),
            None,
        );
    }
    CompleteEnv::with_factory(Cli::command).complete();
}

pub fn run() -> std::io::Result<()> {
    complete_env();

    #[cfg(feature = "eval")]
    if let Some(metrics) = eval::subprocess_flags(&std::env::args().collect::<Vec<_>>()) {
        return gradient_eval::eval_worker::run_eval_worker(metrics);
    }

    let cli = Cli::parse();

    #[cfg(feature = "eval")]
    if matches!(cli.cmd, MainCommands::Eval(_)) {
        let MainCommands::Eval(args) = cli.cmd else {
            unreachable!()
        };
        return crate::commands::eval::run(args);
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_cli(cli))
}

async fn run_cli(cli: Cli) -> std::io::Result<()> {
    let out = Output::new(cli.json);
    store_invocation_overrides_from_cli_and_env(
        cli.account_name,
        cli.project_name,
        cli.task_override,
    );
    crate::session_token_renewal::renew_saved_session_token_if_half_lifetime_passed().await;

    match cli.cmd {
        MainCommands::Account { cmd } => saved_account_commands::handle(cmd, out),
        MainCommands::Completion { shell } => {
            let exe = std::env::current_exe()
                .unwrap_or_else(|e| out.err(ExitKind::Api, format!("cannot locate binary: {e}")));
            let output = Command::new(&exe)
                .env("COMPLETE", shell.to_string())
                .output()
                .unwrap_or_else(|e| {
                    out.err(
                        ExitKind::Api,
                        format!("failed to generate completions: {e}"),
                    )
                });
            let mut stdout = io::stdout();
            stdout.write_all(&output.stdout).ok();
            // clap's dynamic zsh script is registering the completer only when sourced. An fpath
            // autoload file is yielding nothing on the first TAB without this bridge.
            if shell == Shell::Zsh {
                stdout
                    .write_all(
                        b"\n[[ ${funcstack[1]} = _gradient ]] && _clap_dynamic_completer_gradient \"$@\"\n",
                    )
                    .ok();
            }
        }
        MainCommands::Config { key, value } => {
            if let Err(e) = account_name_for_command() {
                out.fail(e);
            }
            match set_or_get_by_name(&key, value, out.is_json()) {
                Ok(value) => out.ok(&serde_json::json!({ key.to_lowercase(): value })),
                Err(e) => out.err(ExitKind::Usage, e),
            }
        }

        MainCommands::Status => {
            let client = client_from_config(out);
            match client.health().await {
                Ok(msg) => {
                    out.ok(&serde_json::json!({"status": "online", "server": msg}));
                    out.human("Server Online.");
                }
                Err(e) => out.fail(e),
            }
        }

        MainCommands::Register {
            username,
            name,
            email,
            password,
        } => {
            if out.is_json() && password.is_none() {
                out.fail(CliError::MissingArgument("--password"));
            }

            server_base(out);

            let input_fields = [("Username", username), ("Name", name), ("Email", email)]
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            let input = handle_input(input_fields, true, out);

            let pw = password.unwrap_or_else(|| ask_for_password(out));

            let client = client_from_config(out);
            match client
                .auth()
                .register(MakeUserRequest {
                    username: input.get("Username"),
                    name: input.get("Name").into(),
                    email: input.get("Email"),
                    password: pw,
                })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"registered": true}));
                    out.human("Registration successful. Please log in.");
                }
                Err(e) => out.fail(e),
            }
        }

        MainCommands::Login {
            server,
            username,
            password,
            no_browser,
        } => {
            if login_name_from_flag_env_or_config(&load_and_migrate_login_config_file()).is_none() {
                out.fail(CliError::NoDefaultAccountSelected);
            }
            let url = server
                .or_else(|| get_value(ConfigKey::Server, true))
                .unwrap_or_else(|| {
                    if out.is_json() {
                        out.fail(CliError::MissingArgument("SERVER"));
                    }
                    ask_for_input("Server URL", out)
                });
            let client = Client::builder()
                .base_url(url.clone())
                .build()
                .unwrap_or_else(|e| out.err(ExitKind::Api, format!("client init failed: {e}")));

            if out.is_json() && username.is_none() {
                out.fail(CliError::MissingArgument("--username"));
            }
            if out.is_json() && password.is_none() {
                out.fail(CliError::MissingArgument("--password"));
            }
            if username.is_some() || password.is_some() {
                let username = username.unwrap_or_else(|| ask_for_input("Username", out));
                let pw = password.unwrap_or_else(|| ask_for_password(out));
                match client
                    .auth()
                    .basic_login(MakeLoginRequest {
                        loginname: username,
                        password: pw,
                        remember_me: None,
                    })
                    .await
                {
                    Ok(token) => {
                        save_logged_in_account_and_start_project_onboarding(url, token, out).await
                    }
                    Err(e) => out.fail(e),
                }
            } else {
                run_web_login(&client, url, out, no_browser).await;
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
                Err(e) => out.fail(e),
            }
        }

        MainCommands::Logout => {
            let file = load_and_migrate_login_config_file();
            let login = login_name_from_flag_env_or_config(&file)
                .and_then(|name| file.accounts.get(&name).cloned())
                .unwrap_or_default();
            let client = login.server().and_then(|server| {
                Client::builder()
                    .base_url(server)
                    .token(login.token()?)
                    .build()
                    .ok()
            });
            let revoked = match client {
                None => false,
                Some(client) => match client.auth().logout().await {
                    Ok(_) => true,
                    Err(e) => {
                        out.human(format!(
                        "Removed the login token from this machine, but failed to revoke it on the server ({e})"
                    ));
                        false
                    }
                },
            };
            set_value(ConfigKey::AuthToken, String::new(), true);
            out.ok(&serde_json::json!({"logged_out": true, "revoked": revoked}));
            out.human("Logged out.");
        }

        MainCommands::LoggedInUser { cmd } => {
            logged_in_user_account_group_commands::handle(cmd, out).await
        }

        MainCommands::Build {
            target,
            system,
            background,
            quiet,
            no_link,
            override_input,
        } => {
            let overrides = build::parse_overrides(&override_input)
                .unwrap_or_else(|msg| out.err(ExitKind::Usage, msg));
            let params = build::BuildParams {
                target,
                system,
                overrides,
                run: None,
            };
            build::handle_build(params, background, quiet, no_link, out).await
        }
        MainCommands::Run(args) => run::handle_run(args, out).await,
        MainCommands::ExecJob { cmd } => exec_job_commands::handle(cmd, out).await,
        MainCommands::Administration { cmd } => admin::handle(cmd, out).await,
        MainCommands::Watch { evaluation } => watch::handle_watch(&evaluation, out).await,
        MainCommands::Logs { evaluation } => logs::handle_logs(&evaluation, out).await,
        MainCommands::Download {
            flake_ref,
            evaluation,
            products,
            out: out_dir,
        } => download::handle_download(flake_ref, evaluation, products, out_dir, out).await,
        MainCommands::Project { cmd } => project::handle(cmd, out).await,
        MainCommands::Task { cmd } => task::handle(cmd, out).await,
        MainCommands::Pr { cmd } => pr::handle(cmd, out).await,
        MainCommands::Worker { cmd } => worker::handle(cmd, out).await,
        MainCommands::Cache { cmd } => cache::handle(cmd, out).await,
        MainCommands::Search { query, limit } => {
            search_command::run_gradient_search_command(query, limit, out).await
        }
        MainCommands::Team { cmd } => team::handle(cmd, out).await,
        MainCommands::Builds { cmd } => builds::handle(cmd, out).await,
        MainCommands::Evaluation { cmd } => evaluation::handle(cmd, out).await,
        MainCommands::Generate { cmd } => generate::handle(cmd, out).await,
        #[cfg(feature = "eval")]
        MainCommands::Eval(_) => unreachable!("eval is handled before the runtime starts"),
        MainCommands::Mcp { control } => {
            if let Err(e) = mcp::run(control).await {
                out.err(ExitKind::Api, format!("mcp server failed: {e}"));
            }
        }
        MainCommands::Hash => {
            let password = ask_for_password(out);
            let confirm = ask_for_password(out);
            if password != confirm {
                out.err(ExitKind::Usage, "Passwords did not match.");
            }
            println!("{}", password_auth::generate_hash(password.as_bytes()));
        }
    }

    std::process::exit(0);
}

async fn save_logged_in_account_and_start_project_onboarding(
    url: String,
    token: String,
    out: Output,
) {
    let client = Client::builder()
        .base_url(url.clone())
        .token(token.clone())
        .build()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("client init failed: {e}")));
    let username = client.user().get().await.ok().map(|user| user.username);

    let (account, saved_project) = update_login_config_file(|file| {
        let account = login_name_from_flag_env_or_config(file)
            .unwrap_or_else(|| out.fail(CliError::NoDefaultAccountSelected));
        let login = file.accounts.entry(account.clone()).or_default();
        let same_server = login
            .server()
            .is_some_and(|saved| saved.trim_end_matches('/') == url.trim_end_matches('/'));
        if !same_server {
            *login = SavedLogin::default();
        }
        login.server = Some(url);
        login.auth_token = Some(token);
        login.username = username.clone();
        let saved_project = login.selected_project.clone();
        file.default_account = Some(account.clone());
        (account, saved_project)
    });

    out.ok(&serde_json::json!({"logged_in": true, "account": account, "username": username}));
    match username {
        Some(username) => out.human(format!(
            "Login succeeded as {username} for account '{account}'."
        )),
        None => out.human("Logged in."),
    }
    project::post_login_project_setup(&client, saved_project.as_deref(), out).await;
}

async fn run_web_login(client: &Client, url: String, out: Output, no_browser: bool) {
    let start = match client.auth().cli_device_start().await {
        Ok(s) => s,
        Err(e) => out.fail(e),
    };

    out.human(format!(
        "Open this URL in your browser:\n  {}\n\nConfirmation code: {}",
        start.verification_uri_complete, start.user_code
    ));

    if !no_browser && !out.is_json() {
        let _ = open_url(&start.verification_uri_complete);
    }

    let interval = Duration::from_secs(start.interval.max(1).unsigned_abs());
    let deadline =
        std::time::Instant::now() + Duration::from_secs(start.expires_in.max(0).unsigned_abs());

    loop {
        if std::time::Instant::now() >= deadline {
            out.fail(CliError::AuthorizationExpired);
        }
        tokio::time::sleep(interval).await;
        match client
            .auth()
            .cli_device_poll(CliDevicePollRequest {
                device_code: start.device_code.clone(),
            })
            .await
        {
            Ok(CliPollOutcome::Pending) => continue,
            Ok(CliPollOutcome::Expired) => {
                out.fail(CliError::AuthorizationExpired);
            }
            Ok(CliPollOutcome::Denied) => {
                out.err(ExitKind::Unauthorized, "Authorization was denied.");
            }
            Ok(CliPollOutcome::Token(token)) => {
                save_logged_in_account_and_start_project_onboarding(url, token, out).await;
                return;
            }
            Err(e) => out.fail(e),
        }
    }
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("open").arg(url).status().map(|_| ())
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("xdg-open").arg(url).status().map(|_| ())
}
