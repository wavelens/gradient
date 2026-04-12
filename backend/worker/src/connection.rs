/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! WebSocket client connection to the Gradient server.
//!
//! [`ProtoConnection`] wraps a `tokio_tungstenite` stream and provides typed
//! send/receive over rkyv-serialised [`proto::messages`] frames.

use anyhow::{Context, Result};
use proto::messages::{ClientMessage, ServerMessage};
use rkyv::rancor::Error as RkyvError;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{debug, instrument, warn};
use futures::SinkExt;
use futures::StreamExt;

/// A live WebSocket connection to the server.
pub struct ProtoConnection {
    /// Protocol version the server reported in `InitAck`. Set to 0 before the
    /// handshake completes; updated via [`Self::set_server_version`] afterwards.
    pub(crate) server_version: u16,
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl ProtoConnection {
    /// Open a WebSocket connection to `url` and return the raw stream.
    /// The caller is responsible for completing the handshake via
    /// [`crate::handshake::perform_handshake`].
    #[instrument(skip_all, fields(%url))]
    pub async fn open(url: &str) -> Result<Self> {
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("failed to connect to {url}"))?;
        Ok(Self { server_version: 0, socket })
    }

    /// Record the protocol version the server reported in `InitAck`.
    pub fn set_server_version(&mut self, version: u16) {
        self.server_version = version;
    }

    /// The protocol version the server reported during the handshake.
    /// Returns 0 if the handshake has not completed yet.
    pub fn server_version(&self) -> u16 {
        self.server_version
    }

    /// Send a typed [`ClientMessage`] to the server.
    pub async fn send(&mut self, msg: ClientMessage) -> Result<()> {
        let bytes = rkyv::to_bytes::<RkyvError>(&msg)
            .context("failed to serialise ClientMessage")?;
        self.socket
            .send(Message::Binary(bytes.to_vec().into()))
            .await
            .context("WebSocket send failed")?;
        debug!("sent client message");
        Ok(())
    }

    /// Receive the next [`ServerMessage`] from the server.
    /// Returns `None` on clean close; errors on protocol violations.
    pub async fn recv(&mut self) -> Result<Option<ServerMessage>> {
        loop {
            match self.socket.next().await {
                None => return Ok(None),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(Message::Binary(bytes))) => {
                    let msg = rkyv::from_bytes::<ServerMessage, RkyvError>(&bytes)
                        .context("failed to deserialise ServerMessage")?;
                    return Ok(Some(msg));
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(other)) => {
                    warn!(?other, "unexpected WebSocket frame type; ignoring");
                }
            }
        }
    }

    /// Close the connection gracefully.
    pub async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }

    /// Reconnect to a (possibly different) URL, resetting the server version.
    /// Used by the main reconnect loop after a clean or unclean disconnect.
    pub async fn reconnect(&mut self, url: &str) -> Result<()> {
        self.close().await;
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("failed to reconnect to {url}"))?;
        self.socket = socket;
        self.server_version = 0;
        Ok(())
    }
}
