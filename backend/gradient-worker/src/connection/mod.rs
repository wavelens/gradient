/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! WebSocket connection management: client connections, handshake, and listener.
//!
//! Framing rides gradient-proto's shared [`ProtoSocket`]/peer-split types; the
//! thin wrappers here only pin the peer role (send `ClientMessage`, receive
//! `ServerMessage`) and remember the server's negotiated protocol version.

pub mod handshake;
pub mod listener;

use anyhow::{Context, Result};
use gradient_proto::messages::{ClientMessage, ServerMessage};
use gradient_proto::session::frame::{ClientWriter, ProtoSocket, ServerReader, accept_tungstenite};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::instrument;

/// Producer-side ceiling for one queued send once the bounded writer queue is
/// full. Exceeding it means the server's TCP receive side stalled; the send
/// fails and the job-level error handling takes over instead of the queue
/// buffering a whole NAR in RAM.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// A live WebSocket connection to the server.
pub struct ProtoConnection {
    /// Protocol version the server reported in `InitAck`. Set to 0 before the
    /// handshake completes; updated via [`Self::set_server_version`] afterwards.
    pub(crate) server_version: u16,
    socket: ProtoSocket,
}

impl ProtoConnection {
    /// Open a WebSocket connection to `url` and return the raw stream.
    /// The caller is responsible for completing the handshake via
    /// [`crate::connection::handshake::perform_handshake`].
    #[instrument(skip_all, fields(%url))]
    pub async fn open(url: &str) -> Result<Self> {
        let socket = gradient_proto::client::dial(url)
            .await
            .with_context(|| format!("failed to connect to {url}"))?;
        Ok(Self {
            server_version: 0,
            socket,
        })
    }

    /// Wrap an already-accepted WebSocket stream (server connected to us).
    pub fn from_accepted(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self {
            server_version: 0,
            socket: accept_tungstenite(socket),
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

    /// Pre-split socket handle for the handshake driver.
    pub(crate) fn socket_mut(&mut self) -> &mut ProtoSocket {
        &mut self.socket
    }

    /// Send a typed [`ClientMessage`] to the server.
    pub async fn send(&mut self, msg: ClientMessage) -> Result<()> {
        self.socket
            .send_client_msg(&msg)
            .await
            .map_err(|_| anyhow::anyhow!("WebSocket send failed"))
    }

    /// Split into a cloneable [`ProtoWriter`] and a [`ProtoReader`], backed by
    /// the shared frame layer's bounded, batch-draining writer task.
    pub fn split(self) -> (ProtoWriter, ProtoReader) {
        let (reader, writer) = self.socket.split_peer(SEND_TIMEOUT);
        (ProtoWriter { inner: writer }, ProtoReader { inner: reader })
    }
}

/// Cloneable write handle over the shared bounded writer queue.
#[derive(Clone)]
pub struct ProtoWriter {
    inner: ClientWriter,
}

impl ProtoWriter {
    /// Enqueue a message for sending. Fails when the writer task has exited
    /// (connection closed) or the queue stayed full past [`SEND_TIMEOUT`]
    /// (server TCP stalled).
    pub async fn send(&self, msg: ClientMessage) -> Result<()> {
        self.inner
            .send_msg(&msg)
            .await
            .map_err(|_| anyhow::anyhow!("writer channel closed or send timed out"))
    }
}

/// Read-only half produced by [`ProtoConnection::split`].
pub struct ProtoReader {
    inner: ServerReader,
}

impl ProtoReader {
    /// Next inbound [`ServerMessage`]; `None` on close or malformed frame.
    pub async fn recv(&mut self) -> Option<ServerMessage> {
        self.inner.recv_msg().await
    }
}
