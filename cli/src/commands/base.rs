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
use clap_complete::engine::ArgValueCompleter;
use clap_complete::{CompleteEnv, Shell};
use connector::auth::{
    CliDevicePollRequest, CliPollOutcome, MakeLoginRequest, MakeUserRequest,
};
use std::io::{self, Write};
use std::process::Command;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
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
        /// Server URL to log in to; sets it as the configured server so a
        /// separate `gradient config server` is not needed.
        server: Option<String>,
        /// Use basic username/password instead of the default web flow
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        password: Option<String>,
        /// Skip opening the browser; print the URL instead
        #[arg(long)]
        no_browser: bool,
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
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_orgs))]
        organization: Option<String>,
        /// Dispatch and return the evaluation UUID without streaming logs
        #[arg(short, long)]
        background: bool,
        #[arg(short, long)]
        quiet: bool,
        /// Do not produce a `result` symlink/folder after the build
        #[arg(long)]
        no_link: bool,
        /// Override a flake input, like `nix build`. Repeatable. REF must be a remote
        /// flake ref (github:, git+ssh://, flake:, ...); local paths are not supported.
        #[arg(long = "override-input", num_args = 2, value_names = ["INPUT", "FLAKE"], action = clap::ArgAction::Append)]
        override_input: Vec<String>,
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
        /// Skip the eval picker; use this evaluation directly
        #[arg(long)]
        evaluation: Option<String>,
        /// Restrict latest-eval lookup to a project (accepts `name` or `org/name`)
        #[arg(long, add = ArgValueCompleter::new(completion::complete_projects))]
        project: Option<String>,
        /// Skip the product picker; comma-separated 1-based indices, ranges (`1-3`), or `all`
        #[arg(long, conflicts_with = "flake_ref")]
        products: Option<String>,
        /// Write to this directory (default: current directory)
        #[arg(long)]
        out: Option<String>,
    },
    /// Inspect builds (dependency graph)
    Builds {
        #[command(subcommand)]
        cmd: builds::Commands,
    },
    /// Generate project files
    Generate {
        #[command(subcommand)]
        cmd: generate::Commands,
    },
    /// Evaluate a flake's outputs to derivations, like nix-eval-jobs
    #[cfg(feature = "eval")]
    Eval(eval::EvalArgs),
    /// Hash a password as an argon2id PHC string for use in
    /// `services.gradient.state.users.<name>.password_file`.
    Hash,
}

/// Intercept dynamic completion requests (`COMPLETE=<shell> gradient …`) and exit.
/// Must run before the tokio runtime starts: completers build their own runtime.
pub fn complete_env() {
    CompleteEnv::with_factory(Cli::command).complete();
}

/// Entry point: parse, then dispatch. `eval` runs synchronously before any
/// runtime starts (the embedded Nix evaluator uses Boehm GC, which must run
/// isolated from Tokio's thread pool); everything else runs on the runtime.
pub fn run() -> std::io::Result<()> {
    complete_env();
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

    match cli.cmd {
        MainCommands::Completion { shell } => {
            let exe = std::env::current_exe()
                .unwrap_or_else(|e| out.err(ExitKind::Api, format!("cannot locate binary: {e}")));
            let output = Command::new(&exe)
                .env("COMPLETE", shell.to_string())
                .output()
                .unwrap_or_else(|e| {
                    out.err(ExitKind::Api, format!("failed to generate completions: {e}"))
                });
            let mut stdout = io::stdout();
            stdout.write_all(&output.stdout).ok();
            // clap's dynamic zsh script registers the completer only when sourced; installed
            // as an fpath autoload file it yields nothing on the first TAB. Bridge the autoload
            // case so the function completes on its first invocation too.
            if shell == Shell::Zsh {
                stdout
                    .write_all(
                        b"\n[[ ${funcstack[1]} = _gradient ]] && _clap_dynamic_completer_gradient \"$@\"\n",
                    )
                    .ok();
            }
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
                    "Server URL not set. Run `gradient login <url>` to set the server and authenticate.",
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

        MainCommands::Login {
            server,
            username,
            password,
            no_browser,
        } => {
            if let Some(url) = server {
                set_get_value(ConfigKey::Server, Some(url), true).unwrap();
            }

            let server_url = set_get_value(ConfigKey::Server, None, true);
            if server_url.is_none() {
                set_get_value(ConfigKey::Server, Some(ask_for_input("Server URL")), true).unwrap();
            }

            if username.is_some() || password.is_some() {
                if out.is_json() && password.is_none() {
                    out.err(ExitKind::Usage, "missing argument: --password");
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
                        organization::post_login_org_setup(&client_from_config(out), out).await;
                    }
                    Err(e) => out.err(to_exit_kind(&e), e),
                }
            } else {
                run_web_login(out, no_browser).await;
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
            background,
            quiet,
            no_link,
            override_input,
        } => {
            let overrides = build::parse_overrides(&override_input)
                .unwrap_or_else(|msg| out.err(ExitKind::Usage, msg));
            let params = build::BuildParams { target, system, overrides };
            build::handle_build(params, organization, background, quiet, no_link, out).await
        }
        MainCommands::Watch { evaluation } => watch::handle_watch(&evaluation, out).await,
        MainCommands::Logs { evaluation } => logs::handle_logs(&evaluation, out).await,
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
        MainCommands::Builds { cmd } => builds::handle(cmd, out).await,
        MainCommands::Generate { cmd } => generate::handle(cmd, out).await,
        #[cfg(feature = "eval")]
        MainCommands::Eval(_) => unreachable!("eval is dispatched before the runtime starts"),
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

async fn run_web_login(out: Output, no_browser: bool) {
    let client = client_from_config(out);
    let start = match client.auth().cli_device_start().await {
        Ok(s) => s,
        Err(e) => out.err(to_exit_kind(&e), e),
    };

    out.human(format!(
        "Open this URL in your browser:\n  {}\n\nConfirmation code: {}",
        start.verification_uri_complete, start.user_code
    ));

    if !no_browser && !out.is_json() {
        let _ = open_url(&start.verification_uri_complete);
    }

    let interval = Duration::from_secs(start.interval.max(1));
    let deadline = std::time::Instant::now()
        + Duration::from_secs(start.expires_in.max(0) as u64);

    loop {
        if std::time::Instant::now() >= deadline {
            out.err(ExitKind::Api, "Authorization expired before approval.");
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
                out.err(ExitKind::Api, "Authorization expired before approval.");
            }
            Ok(CliPollOutcome::Denied) => {
                out.err(ExitKind::Unauthorized, "Authorization was denied.");
            }
            Ok(CliPollOutcome::Token(token)) => {
                set_get_value(ConfigKey::AuthToken, Some(token), true).unwrap();
                out.ok(&serde_json::json!({"logged_in": true}));
                out.human("Logged in.");
                organization::post_login_org_setup(&client_from_config(out), out).await;
                return;
            }
            Err(e) => out.err(to_exit_kind(&e), e),
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
