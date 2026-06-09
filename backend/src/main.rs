/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use gradient_core::init_state;
use gradient_core::types::Cli;
use gradient_core::types::cli::LoggingArgs;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Build an `EnvFilter` directive string from the global default plus optional
/// per-crate overrides. Example output: `info,builder=debug,cache=trace,web=warn`.
fn build_filter_directive(logging: &LoggingArgs) -> String {
    let mut parts = vec![logging.log_level.clone()];
    if let Some(lvl) = &logging.builder_log_level {
        parts.push(format!("builder={}", lvl));
    }
    if let Some(lvl) = &logging.cache_log_level {
        parts.push(format!("cache={}", lvl));
    }
    if let Some(lvl) = &logging.web_log_level {
        parts.push(format!("web={}", lvl));
    }
    if let Some(lvl) = &logging.proto_log_level {
        parts.push(format!("proto={}", lvl));
    }
    parts.join(",")
}

fn init_logging(logging: &LoggingArgs) {
    // `RUST_LOG` always wins if set, so operators can still override at runtime.
    // Otherwise we synthesize a directive from the per-component CLI options.
    let directive = build_filter_directive(logging);
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

pub fn main() -> std::io::Result<()> {
    // Install rustls provider before any TLS handshake (postgres TLS, outbound
    // HTTPS, …) - rustls 0.23 panics otherwise. See issue #232.
    gradient_core::http::init_crypto_provider();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(run())
}

async fn run() -> std::io::Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.logging);

    if cli.storage.validate_state {
        return validate_state_and_exit(cli.storage.state_file.as_deref());
    }

    let state = match init_state(cli).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "server bootstrap failed");
            std::process::exit(1);
        }
    };

    info!(
        version = env!("CARGO_PKG_VERSION"),
        ip = %state.config.server.ip,
        port = state.config.server.port,
        log_level = %state.config.logging.log_level,
        builder_log_level = state.config.logging.builder_log_level.as_deref().unwrap_or("(default)"),
        cache_log_level = state.config.logging.cache_log_level.as_deref().unwrap_or("(default)"),
        web_log_level = state.config.logging.web_log_level.as_deref().unwrap_or("(default)"),
        proto_log_level = state.config.logging.proto_log_level.as_deref().unwrap_or("(default)"),
        "Starting Gradient server"
    );

    let _guard = if state.config.registration.report_errors {
        let dsn = gradient_core::types::cli::effective_sentry_dsn(&state.config.registration);
        info!(dsn, "Error reporting enabled - initializing Sentry");
        Some(sentry::init(dsn.to_string()))
    } else {
        info!("Error reporting disabled");
        None
    };

    info!("Starting cache service");
    gradient_cache::start_cache(Arc::clone(&state)).await?;

    info!("Starting web service");
    gradient_web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}

/// One-shot `--validate-state` action: validate the state file with no DB
/// access and exit non-zero on error so a NixOS build (or CI) fails fast.
fn validate_state_and_exit(state_file: Option<&str>) -> std::io::Result<()> {
    let Some(path) = state_file else {
        eprintln!("--validate-state requires --state-file");
        std::process::exit(2);
    };
    match gradient_core::state::validate_state_file(path) {
        Ok(errors) if errors.is_empty() => {
            println!("State configuration '{path}' is valid");
            Ok(())
        }
        Ok(errors) => {
            eprintln!("State configuration '{path}' is invalid:");
            for e in &errors {
                eprintln!("  - {e}");
            }
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to load state file '{path}': {e}");
            std::process::exit(1);
        }
    }
}
