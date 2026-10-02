/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_wire::session::frame::{BULK_CHUNK_SIZE, MAX_PROTO_MESSAGE_SIZE};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use crate::config::WorkerConfig;
use crate::shutdown::Shutdown;
use crate::worker::Worker;

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

/// Nagle is disabled on each inbound connection.
/// The kernel must not hold back control frames the server is blocked on.
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
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_PROTO_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_PROTO_MESSAGE_SIZE))
        .read_buffer_size(BULK_CHUNK_SIZE);
    let ws = accept_async_with_config(
        tokio_tungstenite::MaybeTlsStream::Plain(stream),
        Some(ws_config),
    )
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
