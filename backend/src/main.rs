/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use gradient_core::init_state;
use gradient_types::Cli;
use gradient_types::cli::LoggingArgs;
use gradient_util::logging::{LogSetup, LogWriter, NOISY_DEPS};
use std::sync::Arc;
use tracing::info;

/// Per-component overrides of the server's log level, keyed by the crates
/// each component lives in.
fn log_overrides(logging: &LoggingArgs) -> [(&'static str, Option<&str>); 7] {
    [
        ("gradient_web", logging.web_log_level.as_deref()),
        ("gradient_cache", logging.cache_log_level.as_deref()),
        ("gradient_proto", logging.proto_log_level.as_deref()),
        ("gradient_wire", logging.proto_log_level.as_deref()),
        (
            "gradient_storage::relay",
            logging.proto_log_level.as_deref(),
        ),
        ("gradient_scheduler", logging.scheduler_log_level.as_deref()),
        ("gradient_pool", logging.scheduler_log_level.as_deref()),
    ]
}

fn init_logging(logging: &LoggingArgs) {
    let overrides = log_overrides(logging);
    gradient_util::logging::init(&LogSetup {
        level: &logging.log_level,
        overrides: &overrides,
        quiet: NOISY_DEPS,
        honor_rust_log: true,
        writer: LogWriter::Stdout,
    });
}

pub fn main() -> std::io::Result<()> {
    // Install rustls provider before any TLS handshake (postgres TLS, outbound
    // HTTPS, …) - rustls 0.23 panics otherwise. See issue #232.
    gradient_util::http::init_crypto_provider();

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
        web_log_level = state.config.logging.web_log_level.as_deref().unwrap_or("(default)"),
        cache_log_level = state.config.logging.cache_log_level.as_deref().unwrap_or("(default)"),
        proto_log_level = state.config.logging.proto_log_level.as_deref().unwrap_or("(default)"),
        scheduler_log_level = state.config.logging.scheduler_log_level.as_deref().unwrap_or("(default)"),
        "Starting Gradient server"
    );

    let _guard = if state.config.registration.report_errors {
        let dsn = gradient_types::cli::effective_sentry_dsn(&state.config.registration);
        info!(dsn, "Error reporting enabled - initializing Sentry");
        Some(sentry::init(dsn.to_string()))
    } else {
        info!("Error reporting disabled");
        None
    };

    state
        .shutdown
        .supervise_now(state.graph.child_spec(state.db()))
        .await
        .map_err(std::io::Error::other)?;
    state
        .shutdown
        .supervise_now(gradient_effects::child_spec(Arc::clone(&state)))
        .await
        .map_err(std::io::Error::other)?;

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
    match gradient_state::validate_state_file(path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn server_directive(logging: &LoggingArgs) -> String {
        let overrides = log_overrides(logging);
        gradient_util::logging::directive(&LogSetup {
            level: &logging.log_level,
            overrides: &overrides,
            quiet: NOISY_DEPS,
            honor_rust_log: true,
            writer: LogWriter::Stdout,
        })
    }

    #[test]
    fn directive_targets_gradient_crates_and_suppresses_noise() {
        let logging = LoggingArgs {
            log_level: "info".into(),
            web_log_level: Some("debug".into()),
            scheduler_log_level: Some("trace".into()),
            ..Default::default()
        };
        let d = server_directive(&logging);
        assert!(d.starts_with("info,"));
        assert!(d.contains("hyper=warn"));
        assert!(d.contains("sqlx=warn"));
        assert!(d.contains("gradient_web=debug"));
        assert!(d.contains("gradient_scheduler=trace"));
        assert!(d.contains("gradient_pool=trace"));
        assert!(!d.contains("builder="));
    }

    #[test]
    fn the_proto_level_reaches_the_relayed_nar_sender() {
        let logging = LoggingArgs {
            proto_log_level: Some("trace".into()),
            ..Default::default()
        };
        let d = server_directive(&logging);
        assert!(d.contains("gradient_storage::relay=trace"), "{d}");
    }

    #[test]
    fn directive_without_overrides_is_global_plus_noise() {
        let d = server_directive(&LoggingArgs::default());
        assert!(d.starts_with("info,"));
        assert!(d.contains("rustls=warn"));
        assert!(!d.contains("gradient_web="));
    }
}
