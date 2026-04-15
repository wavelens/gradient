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
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{instrument, trace, warn};

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
        trace!(?msg, bytes = bytes.len(), "send ClientMessage");
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
                    trace!(?msg, bytes = bytes.len(), "recv ServerMessage");
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

    /// Split into a cloneable [`ProtoWriter`] and a [`ProtoReader`].
    ///
    /// Spawns a background task that drains the writer channel and forwards
    /// messages to the underlying WebSocket sink.  The `ProtoConnection` is
    /// consumed — only the reader half is returned (for `recv` calls).
    pub fn split(self) -> (ProtoWriter, ProtoReader) {
        type BoxSink = std::pin::Pin<Box<dyn futures::Sink<
            Message, Error = tokio_tungstenite::tungstenite::Error
        > + Send>>;
        type BoxStream = std::pin::Pin<Box<dyn futures::Stream<
            Item = Result<Message, tokio_tungstenite::tungstenite::Error>
        > + Send>>;

        let (sink, stream): (BoxSink, BoxStream) = match self.socket {
            ProtoStream::Client(ws) => {
                let (s, r) = ws.split();
                (Box::pin(s), Box::pin(r))
            }
            ProtoStream::Accepted(ws) => {
                let (s, r) = ws.split();
                (Box::pin(s), Box::pin(r))
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMessage>();

        tokio::spawn(async move {
            let mut sink = sink;
            while let Some(msg) = rx.recv().await {
                let bytes = match rkyv::to_bytes::<RkyvError>(&msg) {
                    Ok(b) => b,
                    Err(e) => { tracing::warn!("serialisation error: {}", e); continue; }
                };
                if let Err(e) = SinkExt::send(&mut sink, Message::Binary(bytes.to_vec().into())).await {
                    tracing::warn!("WebSocket write error: {}", e);
                    break;
                }
                tracing::trace!(?msg, bytes = bytes.len(), "send ClientMessage (split)");
            }
        });

        (
            ProtoWriter { tx },
            ProtoReader { stream: Box::pin(stream) },
        )
    }
}

/// Cloneable write handle backed by an unbounded mpsc channel.
/// The drain task is spawned by [`ProtoConnection::split`].
#[derive(Clone)]
pub struct ProtoWriter {
    tx: mpsc::UnboundedSender<ClientMessage>,
}

impl ProtoWriter {
    /// Enqueue a message for sending. Returns Err only if the writer
    /// task has exited (connection closed).
    pub fn send(&self, msg: ClientMessage) -> anyhow::Result<()> {
        self.tx.send(msg).map_err(|_| anyhow::anyhow!("writer channel closed"))
    }
}

/// Read-only half produced by [`ProtoConnection::split`].
pub struct ProtoReader {
    stream: std::pin::Pin<Box<dyn futures::Stream<
        Item = Result<tokio_tungstenite::tungstenite::Message,
                      tokio_tungstenite::tungstenite::Error>
    > + Send>>,
}

impl ProtoReader {
    pub async fn recv(&mut self) -> anyhow::Result<Option<ServerMessage>> {
        loop {
            match self.stream.next().await {
                None => return Ok(None),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => {
                    let mut aligned = rkyv::util::AlignedVec::<16>::new();
                    aligned.extend_from_slice(&bytes);
                    let msg = rkyv::from_bytes::<ServerMessage, rkyv::rancor::Error>(&aligned)
                        .context("failed to deserialise ServerMessage")?;
                    tracing::trace!(?msg, bytes = bytes.len(), "recv ServerMessage (reader)");
                    return Ok(Some(msg));
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_)))
                | Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => continue,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => return Ok(None),
                Some(Ok(other)) => {
                    warn!(?other, "unexpected WebSocket frame type; ignoring");
                }
            }
        }
    }
}
