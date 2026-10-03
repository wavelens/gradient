/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod handshake;

use anyhow::{Context, Result};
use gradient_wire::messages::{ClientMessage, ServerMessage};
use gradient_wire::session::frame::{ClientWriter, ProtoSocket, ServerReader, accept_tungstenite};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::instrument;

const SEND_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ProtoConnection {
    socket: ProtoSocket,
}

impl ProtoConnection {
    #[instrument(skip_all, fields(%url))]
    pub async fn open(url: &str) -> Result<Self> {
        let mut socket = gradient_wire::client::dial(url)
            .await
            .with_context(|| format!("failed to connect to {url}"))?;

        if socket.agree_version().await.is_none() {
            let reason = socket
                .refusal()
                .unwrap_or("connection closed during version agreement");
            anyhow::bail!("{url}: {reason}");
        }

        Ok(Self { socket })
    }

    pub fn from_accepted(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self {
            socket: accept_tungstenite(socket),
        }
    }

    pub fn socket_mut(&mut self) -> &mut ProtoSocket {
        &mut self.socket
    }

    pub async fn send(&mut self, msg: ClientMessage) -> Result<()> {
        self.socket
            .send_client_msg(&msg)
            .await
            .map_err(|_| anyhow::anyhow!("WebSocket send failed"))
    }

    pub fn split(self) -> (ProtoWriter, ProtoReader, WriterFlush) {
        let (reader, writer, task) = self.socket.split_peer(SEND_TIMEOUT);
        (
            ProtoWriter { inner: writer },
            ProtoReader { inner: reader },
            WriterFlush(task),
        )
    }
}

pub struct WriterFlush(tokio::task::JoinHandle<()>);

impl WriterFlush {
    /// A send is only enqueuing, and a worker exiting right after its last report would drop it.
    /// The budget is bounding the wait while a background task is still holding a writer clone.
    pub async fn flush(self, budget: Duration) {
        if tokio::time::timeout(budget, self.0).await.is_err() {
            tracing::warn!(
                budget_secs = budget.as_secs(),
                "writer did not flush in time; the server may not have every final report"
            );
        }
    }

    /// Background tasks may still hold writer clones. The session would stay open with nobody
    /// reading it, and the server would refuse the reconnect.
    pub fn close(self) {
        self.0.abort();
    }
}

#[derive(Clone)]
pub struct ProtoWriter {
    inner: ClientWriter,
}

impl ProtoWriter {
    pub async fn send(&self, msg: ClientMessage) -> Result<()> {
        self.inner
            .send_msg(&msg)
            .await
            .map_err(|_| WriterUnavailable.into())
    }
}

#[derive(Debug)]
pub struct WriterUnavailable;

impl std::fmt::Display for WriterUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("writer channel closed or send timed out")
    }
}
impl std::error::Error for WriterUnavailable {}

#[derive(Debug)]
pub struct Unresponsive;

impl std::fmt::Display for Unresponsive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the server or object store did not answer in time")
    }
}
impl std::error::Error for Unresponsive {}

pub struct ProtoReader {
    inner: ServerReader,
}

impl ProtoReader {
    pub async fn recv(&mut self) -> Option<ServerMessage> {
        self.inner.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::testing::MockProtoServer;

    #[tokio::test]
    async fn closing_the_writer_drops_the_socket_while_a_background_clone_lives() {
        let server = MockProtoServer::bind().await;
        let (mut sc, conn) = tokio::join!(server.accept(), ProtoConnection::open(server.url()));
        let conn = conn.unwrap();

        let (writer, reader, flush) = conn.split();
        let background = writer.clone();
        drop((writer, reader));
        flush.close();

        let seen = tokio::time::timeout(Duration::from_secs(5), sc.recv())
            .await
            .expect("the server must see the socket close, not wait on a dead session");
        assert!(seen.is_err());
        assert!(
            background
                .send(ClientMessage::RequestJobList)
                .await
                .is_err()
        );
    }
}
