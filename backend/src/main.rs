/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use gradient_core::init_state;
use gradient_core::types::Cli;
use std::io::{self, IsTerminal, Read};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Build an `EnvFilter` directive string from the global default plus optional
/// per-crate overrides. Example output: `info,builder=debug,cache=trace,web=warn`.
fn build_filter_directive(cli: &Cli) -> String {
    let mut parts = vec![cli.log_level.clone()];
    if let Some(lvl) = &cli.builder_log_level {
        parts.push(format!("builder={}", lvl));
    }
    if let Some(lvl) = &cli.cache_log_level {
        parts.push(format!("cache={}", lvl));
    }
    if let Some(lvl) = &cli.web_log_level {
        parts.push(format!("web={}", lvl));
    }
    if let Some(lvl) = &cli.proto_log_level {
        parts.push(format!("proto={}", lvl));
    }
    parts.join(",")
}

fn init_logging(cli: &Cli) {
    // `RUST_LOG` always wins if set, so operators can still override at runtime.
    // Otherwise we synthesize a directive from the per-component CLI options.
    let directive = build_filter_directive(cli);
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(e) => {
            eprintln!(
                "Warning: Invalid RUST_LOG environment variable ({}), using configured log level: {}",
                e, directive
            );
            EnvFilter::new(&directive)
        }
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .with(env_filter)
        .init();
}

/// Read a password from stdin (piped) or prompt interactively without echo.
fn read_password() -> std::io::Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        let pw = rpassword::prompt_password("Password: ")?;
        let confirm = rpassword::prompt_password("Confirm:  ")?;
        if pw != confirm {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "passwords did not match",
            ));
        }
        Ok(pw)
    } else {
        let mut buf = String::new();
        stdin.lock().read_to_string(&mut buf)?;
        // Strip a single trailing newline if present (so `echo pw | ...` works).
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        Ok(buf)
    }
}

/// Run the `hash` subcommand: read a password and print an argon2id PHC hash.
fn run_hash_command() -> std::io::Result<()> {
    let password = read_password()?;
    if password.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty password",
        ));
    }
    let hash = password_auth::generate_hash(password.as_bytes());
    println!("{hash}");
    Ok(())
}

pub fn main() -> std::io::Result<()> {
    // Lightweight subcommand handling: `gradient-server hash` produces an
    // argon2id PHC hash for use in `services.gradient.state.users.<name>.password_file`.
    let mut args = std::env::args().skip(1);
    if let Some(first) = args.next() {
        match first.as_str() {
            "hash" => return run_hash_command(),
            "--help" | "-h" | "--version" | "-V" => {} // fall through to clap
            _ => {}
        }
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(run())
}

async fn run() -> std::io::Result<()> {
    let cli = Cli::parse();
    let state = init_state(cli).await;

    // Initialize logging with the configured level
    init_logging(&state.cli);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        ip = %state.cli.ip,
        port = state.cli.port,
        log_level = %state.cli.log_level,
        builder_log_level = state.cli.builder_log_level.as_deref().unwrap_or("(default)"),
        cache_log_level = state.cli.cache_log_level.as_deref().unwrap_or("(default)"),
        web_log_level = state.cli.web_log_level.as_deref().unwrap_or("(default)"),
        proto_log_level = state.cli.proto_log_level.as_deref().unwrap_or("(default)"),
        "Starting Gradient server"
    );

    let _guard = if state.cli.report_errors {
        info!("Error reporting enabled - initializing Sentry");
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        info!("Error reporting disabled");
        None
    };

    info!("Starting cache service");
    cache::start_cache(Arc::clone(&state)).await?;

    info!("Starting web service");
    web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}
