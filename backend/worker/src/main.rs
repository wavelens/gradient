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

use anyhow::Result;
use clap::Parser;
use tracing::info;

use config::WorkerConfig;
use worker::Worker;

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
        let mut worker = Worker::connect(config).await?;
        worker.run().await
    })
}
