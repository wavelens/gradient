/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;
mod connection;
mod credentials;
mod eval_worker;
mod executor;
mod flake;
mod handshake;
mod job;
mod nar;
mod nix_eval;
mod scorer;
mod store;
mod traits;
mod worker;
mod worker_pool;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{error, info, warn};

use config::WorkerConfig;
use worker::Worker;

/// Maximum delay between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Initial delay after the first disconnect.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let config = WorkerConfig::parse();

    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();

    // Re-exec as eval subprocess when launched with the internal flag.
    // The Nix C API (Boehm GC) must run single-threaded, isolated from Tokio.
    if config.eval_worker {
        return eval_worker::run_eval_worker().map_err(anyhow::Error::from);
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        info!(server_url = %config.server_url, "gradient-worker starting");

        let mut backoff = INITIAL_BACKOFF;

        // Initial connection.
        let mut worker = loop {
            match Worker::connect(config.clone()).await {
                Ok(w) => {
                    backoff = INITIAL_BACKOFF;
                    break w;
                }
                Err(e) => {
                    error!(error = %e, delay_secs = backoff.as_secs(), "connection failed; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        };

        // Run → reconnect loop.
        loop {
            match worker.run().await {
                Ok(()) if worker.is_draining() => {
                    info!("server requested drain; shutting down");
                    break;
                }
                Ok(()) => {
                    warn!(delay_secs = backoff.as_secs(), "connection closed; reconnecting");
                }
                Err(e) => {
                    error!(error = %e, delay_secs = backoff.as_secs(), "dispatch loop error; reconnecting");
                }
            }

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);

            match worker.reconnect().await {
                Ok(()) => {
                    info!("reconnected successfully");
                    backoff = INITIAL_BACKOFF;
                }
                Err(e) => {
                    error!(error = %e, "reconnect failed; will retry");
                }
            }
        }

        Ok(())
    })
}
