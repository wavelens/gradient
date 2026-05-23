/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;
mod connection;
mod connection_state;
mod executor;
mod http;
mod nix;
mod proto;
mod reconnect;
mod traits;
mod worker;
mod worker_pool;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use config::WorkerConfig;
use connection_state::RunOutcome;
use reconnect::retry_reconnect;
use tracing_subscriber::EnvFilter;
use worker::Worker;

/// Maximum delay between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Initial delay after the first disconnect.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let config = WorkerConfig::parse();

    // stderr, not stdout: eval-worker subprocesses use stdout for line-delimited
    // JSON to the parent (see worker_pool::pool), so any tracing line on stdout
    // would be parsed as a protocol response and crash the eval.
    tracing_subscriber::fmt()
        .with_env_filter(build_env_filter(&config))
        .with_writer(std::io::stderr)
        .init();

    // Re-exec as eval subprocess when launched with the internal flag.
    // The Nix C API (Boehm GC) must run single-threaded, isolated from Tokio.
    if config.eval_worker {
        return nix::eval_worker::run_eval_worker().map_err(anyhow::Error::from);
    }

    // Must precede the first TLS handshake - `connect_async` for `wss://` is
    // the first thing the runtime does and rustls 0.23 panics if no provider
    // is installed (see issue #232).
    gradient_core::http::init_crypto_provider();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        info!(server_url = %config.server_url, "gradient-worker starting");

        if !config.server_url.contains("proto") {
            warn!(
                server_url = %config.server_url,
                "server URL does not contain 'proto'; expected the server's /proto WebSocket endpoint (e.g. wss://gradient.example.com/proto)"
            );
        }

        let shutdown = CancellationToken::new();
        install_signal_handler(shutdown.clone());

        // Start the listener for incoming server connections if discoverable.
        if config.discoverable {
            let listener_config = config.clone();
            let listener_shutdown = shutdown.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    connection::listener::start_listener(listener_config, listener_shutdown).await
                {
                    error!(error = %e, "listener failed");
                }
            });
        }

        let mut backoff = INITIAL_BACKOFF;

        // Initial connection - abandon retries if shutdown fires.
        let initial = tokio::select! {
            _ = shutdown.cancelled() => None,
            w = async {
                loop {
                    match Worker::connect(config.clone()).await {
                        Ok(w) => break w,
                        Err(e) => {
                            error!(
                                error = %e,
                                delay_secs = backoff.as_secs(),
                                "connection failed; retrying"
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                        }
                    }
                }
            } => Some(w),
        };
        let Some(mut worker) = initial else {
            // Signal arrived before we connected: nothing to drain.
            info!("shutdown requested during initial connect");
            return Ok(());
        };
        backoff = INITIAL_BACKOFF;

        // Cache an executor handle so we can gracefully drain the eval pool
        // even after `worker` is consumed by `run` / `reconnect`. The handle
        // shares the underlying `Arc<WorkerEvaluator>` so the pool stays
        // alive until the last clone is dropped.
        let executor_handle = worker.executor_handle();

        // Run → reconnect loop.
        loop {
            let (disconnected, outcome) = worker.run(shutdown.clone()).await;

            // Shutdown signalled during run(): exit before attempting reconnect.
            if shutdown.is_cancelled() {
                info!("shutdown signal received; tearing down worker");
                drop(disconnected);
                executor_handle.shutdown().await;
                return Ok(());
            }

            match outcome {
                Ok(RunOutcome::Drained) => {
                    info!("server requested drain; shutting down");
                    drop(disconnected);
                    executor_handle.shutdown().await;
                    return Ok(());
                }
                Ok(RunOutcome::CleanDisconnect) => {
                    warn!(delay_secs = backoff.as_secs(), "connection closed; reconnecting");
                }
                Err(e) => {
                    error!(error = %e, delay_secs = backoff.as_secs(), "dispatch loop error; reconnecting");
                }
            }

            // Reconnect with exponential backoff, but bail out if shutdown
            // fires while we're waiting. Never give up otherwise - a transient
            // network blip must not kill the worker. The disconnected handle
            // is consumed by retry_reconnect; if shutdown cancels the future
            // mid-attempt the cached `executor_handle` still drives the
            // graceful pool shutdown.
            let reconnected = tokio::select! {
                _ = shutdown.cancelled() => None,
                w = retry_reconnect(
                    disconnected,
                    |d| async move { d.reconnect().await },
                    |delay| tokio::time::sleep(delay),
                    backoff,
                    MAX_BACKOFF,
                ) => Some(w),
            };
            match reconnected {
                Some(w) => {
                    info!("reconnected successfully");
                    worker = w;
                    backoff = INITIAL_BACKOFF;
                }
                None => {
                    info!("shutdown requested during reconnect");
                    executor_handle.shutdown().await;
                    return Ok(());
                }
            }
        }
    })
}

/// Install a SIGINT/SIGTERM handler that cancels `shutdown` so the run loop
/// can break out and call [`Worker::shutdown`] before exit.
fn install_signal_handler(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "failed to install SIGTERM handler");
                    return;
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("received SIGINT, shutting down"),
                _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl-C, shutting down");
        }
        shutdown.cancel();
    });
}

/// Build the tracing `EnvFilter` from the worker's log-level config.
///
/// `log_level` is the global default. The optional per-area overrides
/// (`eval_log_level`, `build_log_level`, `proto_log_level`) are appended as
/// per-target directives so e.g. `settings.logLevel.eval = "trace"` enables
/// trace logging only for the evaluator-related modules.
fn build_env_filter(config: &WorkerConfig) -> EnvFilter {
    const EVAL_TARGETS: &[&str] = &[
        "gradient_worker::nix",
        "gradient_worker::worker_pool",
        "gradient_worker::executor::eval",
    ];
    const BUILD_TARGETS: &[&str] = &[
        "gradient_worker::executor::build",
        "gradient_worker::executor::compress",
    ];
    const PROTO_TARGETS: &[&str] = &[
        "gradient_worker::proto",
        "gradient_worker::connection",
        "gradient_worker::connection_state",
    ];

    let mut filter = EnvFilter::new(&config.log_level);
    let mut overrides: Vec<(&str, &str)> = Vec::new();
    if let Some(lvl) = &config.eval_log_level {
        overrides.extend(EVAL_TARGETS.iter().map(|t| (*t, lvl.as_str())));
    }
    if let Some(lvl) = &config.build_log_level {
        overrides.extend(BUILD_TARGETS.iter().map(|t| (*t, lvl.as_str())));
    }
    if let Some(lvl) = &config.proto_log_level {
        overrides.extend(PROTO_TARGETS.iter().map(|t| (*t, lvl.as_str())));
    }
    for (target, lvl) in overrides {
        match format!("{target}={lvl}").parse() {
            Ok(d) => filter = filter.add_directive(d),
            Err(e) => eprintln!("invalid log directive {target}={lvl}: {e}"),
        }
    }
    filter
}
