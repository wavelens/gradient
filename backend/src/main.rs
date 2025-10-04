/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use core::init_state;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

fn init_logging(log_level: &str, debug: bool) {
    // SQL logging is now controlled at the database connection level
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(e) => {
            eprintln!("Warning: Invalid RUST_LOG environment variable ({}), using default log level: {}", e, log_level);
            EnvFilter::new(log_level)
        }
    };

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_file(debug)
                .with_line_number(debug),
        )
        .with(env_filter)
        .init();
}

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let state = init_state().await;

    // Initialize logging with the configured level
    init_logging(&state.cli.log_level, state.cli.debug);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        ip = %state.cli.ip,
        port = state.cli.port,
        debug = state.cli.debug,
        log_level = %state.cli.log_level,
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

    info!("Starting builder service");
    builder::start_builder(Arc::clone(&state)).await?;

    info!("Starting cache service");
    cache::start_cache(Arc::clone(&state)).await?;

    info!("Starting web service");
    web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}
