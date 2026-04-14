/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! WebSocket connection management: client connections, handshake, and listener.

pub mod handshake;
pub mod listener;

use anyhow::{Context, Result};
use futures::SinkExt;
use futures::StreamExt;
use proto::messages::{ClientMessage, ServerMessage};
use rkyv::rancor::Error as RkyvError;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{debug, instrument, warn};

/// Internal enum to abstract over client-initiated and server-accepted sockets.
enum ProtoStream {
    /// Outbound: worker connected to a server URL.
    Client(WebSocketStream<MaybeTlsStream<TcpStream>>),
    /// Inbound: server connected to our listener.
    Accepted(WebSocketStream<TcpStream>),
}

/// A live WebSocket connection to the server.
pub struct ProtoConnection {
    /// Protocol version the server reported in `InitAck`. Set to 0 before the
    /// handshake completes; updated via [`Self::set_server_version`] afterwards.
    pub(crate) server_version: u16,
    socket: ProtoStream,
}

impl ProtoConnection {
    /// Open a WebSocket connection to `url` and return the raw stream.
    /// The caller is responsible for completing the handshake via
    /// [`crate::connection::handshake::perform_handshake`].
    #[instrument(skip_all, fields(%url))]
    pub async fn open(url: &str) -> Result<Self> {
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("failed to connect to {url}"))?;
        Ok(Self {
            server_version: 0,
            socket: ProtoStream::Client(socket),
        })
    }

    /// Wrap an already-accepted WebSocket stream (server connected to us).
    pub fn from_accepted(socket: WebSocketStream<TcpStream>) -> Self {
        Self {
            server_version: 0,
            socket: ProtoStream::Accepted(socket),
        }
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
        let bytes =
            rkyv::to_bytes::<RkyvError>(&msg).context("failed to serialise ClientMessage")?;
        let frame = Message::Binary(bytes.to_vec().into());
        match &mut self.socket {
            ProtoStream::Client(ws) => ws.send(frame).await.context("WebSocket send failed")?,
            ProtoStream::Accepted(ws) => ws.send(frame).await.context("WebSocket send failed")?,
        }
        debug!("sent client message");
        Ok(())
    }

    /// Receive the next [`ServerMessage`] from the server.
    /// Returns `None` on clean close; errors on protocol violations.
    pub async fn recv(&mut self) -> Result<Option<ServerMessage>> {
        loop {
            let frame = match &mut self.socket {
                ProtoStream::Client(ws) => ws.next().await,
                ProtoStream::Accepted(ws) => ws.next().await,
            };
            match frame {
                None => return Ok(None),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(Message::Binary(bytes))) => {
                    let mut aligned = rkyv::util::AlignedVec::<16>::new();
                    aligned.extend_from_slice(&bytes);
                    let msg = rkyv::from_bytes::<ServerMessage, RkyvError>(&aligned)
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
        match &mut self.socket {
            ProtoStream::Client(ws) => {
                let _ = ws.close(None).await;
            }
            ProtoStream::Accepted(ws) => {
                let _ = ws.close(None).await;
            }
        }
    }

    /// Reconnect to a (possibly different) URL, resetting the server version.
    /// Used by the main reconnect loop after a clean or unclean disconnect.
    pub async fn reconnect(&mut self, url: &str) -> Result<()> {
        self.close().await;
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("failed to reconnect to {url}"))?;
        self.socket = ProtoStream::Client(socket);
        self.server_version = 0;
        Ok(())
    }
}
