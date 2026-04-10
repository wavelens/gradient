/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use evaluator::{WorkerPoolResolver, run_eval_worker};
use gradient_core::nix::DerivationResolver;
use gradient_core::init_state;
use gradient_core::types::Cli;
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

pub fn main() -> std::io::Result<()> {
    // When invoked as a child eval worker, run the synchronous worker loop
    // and exit. The worker speaks JSON over stdin/stdout to its parent and
    // does not need a tokio runtime, a database, or any other server services.
    if std::env::args().any(|a| a == "--eval-worker") {
        return run_eval_worker();
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

    // Drive a pool of long-lived eval-worker subprocesses (one persistent
    // `NixEvaluator` each). Each worker is single-threaded, isolating the
    // thread-unsafe Nix C API and avoiding Boehm GC ↔ Tokio conflicts.
    let derivation_resolver: Arc<dyn DerivationResolver> = Arc::new(WorkerPoolResolver::new(
        cli.eval_workers,
        cli.max_evaluations_per_worker,
    ));

    let state = init_state(cli, derivation_resolver).await;

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

    info!("Starting evaluator service");
    evaluator::start_evaluator(Arc::clone(&state)).await?;

    info!("Starting builder service");
    builder::start_builder(Arc::clone(&state)).await?;

    info!("Starting cache service");
    cache::start_cache(Arc::clone(&state)).await?;

    info!("Starting web service");
    web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}
