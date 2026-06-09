/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Mock WebSocket server for testing `ProtoConnection`-based worker code.
//!
//! [`MockProtoServer`] binds a TCP listener on `127.0.0.1:0`, hands out the
//! allocated `ws://` URL, and accepts one WebSocket connection per call to
//! [`MockProtoServer::accept`].  The resulting [`MockServerConn`] exposes typed
//! `send` / `recv` that match [`worker::connection::ProtoConnection`]'s wire
//! format (rkyv-serialized binary frames).

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use gradient_proto::messages::{ClientMessage, ServerMessage, decode_client_message};
use rkyv::rancor::Error as RkyvError;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async};

/// A mock WebSocket server bound to a random local port.
pub struct MockProtoServer {
    listener: TcpListener,
    url: String,
}

impl MockProtoServer {
    /// Bind to `127.0.0.1:0` and return the server.
    pub async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind mock server");
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");
        Self { listener, url }
    }

    /// The `ws://` URL to pass to `ProtoConnection::open`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Accept one incoming WebSocket connection.
    pub async fn accept(&self) -> MockServerConn {
        let (stream, _) = self.listener.accept().await.expect("accept failed");
        let socket = accept_async(stream)
            .await
            .expect("WebSocket handshake failed");
        MockServerConn { socket }
    }
}

/// Server-side endpoint of a mock WebSocket connection.
pub struct MockServerConn {
    socket: WebSocketStream<TcpStream>,
}

impl MockServerConn {
    /// Send a [`ServerMessage`] to the connected client.
    pub async fn send(&mut self, msg: ServerMessage) -> Result<()> {
        let bytes =
            rkyv::to_bytes::<RkyvError>(&msg).context("failed to serialise ServerMessage")?;
        self.socket
            .send(Message::Binary(bytes.to_vec().into()))
            .await
            .context("mock server WebSocket send failed")
    }

    /// Receive the next [`ClientMessage`] from the connected client.
    /// Skips ping/pong frames transparently.
    pub async fn recv(&mut self) -> Result<ClientMessage> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    return decode_client_message(&bytes)
                        .context("failed to deserialise ClientMessage");
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) => {
                    anyhow::bail!("client closed the connection")
                }
                Some(Ok(other)) => {
                    anyhow::bail!("unexpected frame: {:?}", other)
                }
                Some(Err(e)) => return Err(e.into()),
                None => anyhow::bail!("connection closed without message"),
            }
        }
    }

    /// Close the server side gracefully.
    pub async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
}
