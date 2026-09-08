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
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use crate::config::WorkerConfig;
use crate::shutdown::Shutdown;
use crate::worker::Worker;

/// Start listening for incoming server connections on the configured port.
///
/// Each accepted connection gets its own executor and dispatch loop, running
/// concurrently with the worker's outbound connection (if any). `shutdown` is
/// observed by both the accept loop and each per-connection dispatch loop so
/// inbound sessions drain their in-flight jobs and eval pools on signal;
/// `sessions` is how the run loop waits for that drain before the process
/// exits out from under them.
pub async fn start_listener(
    config: WorkerConfig,
    shutdown: Shutdown,
    sessions: TaskTracker,
) -> Result<()> {
    let addr = format!("{}:{}", config.listen_addr, config.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind listener on {addr}"))?;
    info!(addr = %addr, "listening for incoming server connections");

    loop {
        tokio::select! {
            _ = shutdown.drain_requested() => {
                info!("shutdown requested; closing inbound listener");
                return Ok(());
            }
            accept = accept_tuned(&listener) => match accept {
                Ok((stream, addr)) => {
                    info!(%addr, "incoming connection accepted");
                    let config = config.clone();
                    let conn_shutdown = shutdown.clone();
                    sessions.spawn(async move {
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

/// Accept one inbound connection with Nagle disabled, so the control frames
/// the server is blocked on are not held back by the kernel.
async fn accept_tuned(
    listener: &TcpListener,
) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
    let (stream, addr) = listener.accept().await?;
    gradient_util::net::disable_nagle(&stream);
    Ok((stream, addr))
}

async fn handle_incoming(
    stream: tokio::net::TcpStream,
    config: WorkerConfig,
    shutdown: Shutdown,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reverts if `accept_tuned` stops calling `disable_nagle`: an inbound
    /// server connection carries the same latency-critical control frames as
    /// an outbound one and must be tuned the same way.
    #[tokio::test]
    async fn accept_tuned_disables_nagle_on_the_inbound_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let (accepted, client) = tokio::join!(
            accept_tuned(&listener),
            tokio::net::TcpStream::connect(addr)
        );
        let (stream, _peer) = accepted.expect("accept");
        let _client = client.expect("connect");

        assert!(stream.nodelay().expect("read nodelay"));
    }
}
