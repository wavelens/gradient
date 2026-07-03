/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! WebSocket listener for incoming server connections.
//!
//! When `discoverable = true`, the worker starts a TCP listener and accepts
//! incoming WebSocket upgrades.  Each accepted connection runs the same
//! handshake and dispatch loop as an outbound connection - the protocol is
//! identical regardless of who initiated the transport.

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::WorkerConfig;
use crate::worker::Worker;

/// Start listening for incoming server connections on the configured port.
///
/// Each accepted connection gets its own executor and dispatch loop, running
/// concurrently with the worker's outbound connection (if any). `shutdown` is
/// observed by both the accept loop and each per-connection dispatch loop so
/// inbound workers gracefully drain their eval pools on signal.
pub async fn start_listener(config: WorkerConfig, shutdown: CancellationToken) -> Result<()> {
    let addr = format!("{}:{}", config.listen_addr, config.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind listener on {addr}"))?;
    info!(addr = %addr, "listening for incoming server connections");

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("shutdown requested; closing inbound listener");
                return Ok(());
            }
            accept = listener.accept() => match accept {
                Ok((stream, addr)) => {
                    info!(%addr, "incoming connection accepted");
                    let config = config.clone();
                    let conn_shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_incoming(stream, config, conn_shutdown).await {
                            error!(%addr, error = %e, "incoming connection failed");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "listener accept error");
                }
            }
        }
    }
}

async fn handle_incoming(
    stream: tokio::net::TcpStream,
    config: WorkerConfig,
    shutdown: CancellationToken,
) -> Result<()> {
    let ws = accept_async(tokio_tungstenite::MaybeTlsStream::Plain(stream))
        .await
        .context("WebSocket upgrade failed")?;

    let worker = Worker::from_accepted(ws, config).await?;
    let executor_handle = worker.executor_handle();
    let (_disconnected, outcome) = worker.run(shutdown).await;
    executor_handle.shutdown().await;
    outcome.map(|_| ())
}
