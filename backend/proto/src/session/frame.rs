/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Frame-level I/O over a generic WebSocket transport.
//!
//! After the handshake completes, the connection is split into a single-owner
//! [`ProtoReader`] (used by the dispatch loop) and a cloneable [`ProtoWriter`]
//! (mpsc-backed, drained by a spawned writer task). Splitting decouples reads
//! from writes so a slow outbound NAR transfer cannot block inbound message
//! handling, and lets concurrent NAR-serving tasks share the wire safely.

use std::time::Duration;

use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use rkyv::rancor::Error as RkyvError;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, tungstenite::Message as TungsteniteMessage,
};
use tracing::{debug, trace, warn};

use crate::messages::{ClientMessage, ServerMessage, decode_client_message};

// ── Constants ─────────────────────────────────────────────────────────────────

pub const JOB_OFFER_CHUNK_SIZE: usize = 1_000;
pub const NAR_PUSH_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Hard upper bound on any single inbound or outbound `/proto` WebSocket
/// frame/message. Comfortably above the largest legitimate frame
/// (`NarPush` carries 4 MiB chunks plus rkyv overhead and metadata) while
/// preventing a peer from pinning gigabytes of memory with a single send.
/// Applied to both the inbound axum upgrade and the outbound tungstenite
/// connect.
pub const MAX_PROTO_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

/// Maximum time the server will wait for a peer to complete the handshake
/// (Discoverable check → InitConnection → AuthChallenge → AuthResponse →
/// InitAck). A peer that opens the WebSocket and then stalls is dropped after
/// this deadline so it cannot pin a tokio task and FD indefinitely.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Bounded queue depth for [`ProtoWriter`]. With a 4 MiB NAR chunk ceiling
/// (`NAR_PUSH_CHUNK_SIZE`) this caps the per-connection outbound buffer at
/// roughly `WRITER_QUEUE_DEPTH * NAR_PUSH_CHUNK_SIZE` ≈ 64 MiB. Producers
/// observe back-pressure as `tx.send().await` blocking, which is then capped
/// by the per-message send timeout passed to [`ProtoSocket::split`].
const WRITER_QUEUE_DEPTH: usize = 16;

// ── Socket abstraction ────────────────────────────────────────────────────────

/// Wraps both axum and raw tungstenite WebSocket streams so handshake code
/// can drive connections regardless of who initiated the transport. After the
/// handshake completes, [`Self::split`] consumes the socket and hands back a
/// reader + cloneable writer pair for the dispatch phase.
pub enum ProtoSocket {
    /// Inbound: worker connected to the server's `/proto` endpoint.
    /// Boxed alongside `Tungstenite` so neither variant pads the other.
    Axum(Box<WebSocket>),
    /// Outbound: server connected to a worker's listener.
    /// Boxed so the TLS state (≈1.4 KB) doesn't pad every other variant.
    Tungstenite(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl ProtoSocket {
    async fn recv_bytes(&mut self) -> Option<Result<Vec<u8>, ()>> {
        match self {
            Self::Axum(ws) => loop {
                match ws.recv().await? {
                    Ok(AxumMessage::Binary(bytes)) => return Some(Ok(bytes.to_vec())),
                    Ok(AxumMessage::Close(_)) => return None,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
            Self::Tungstenite(ws) => loop {
                match ws.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => return Some(Ok(bytes.to_vec())),
                    Ok(TungsteniteMessage::Close(_)) => return None,
                    Ok(TungsteniteMessage::Ping(_) | TungsteniteMessage::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
        }
    }

    async fn send_bytes(&mut self, bytes: Vec<u8>) -> Result<(), ()> {
        match self {
            Self::Axum(ws) => ws
                .send(AxumMessage::Binary(bytes.into()))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
            Self::Tungstenite(ws) => ws
                .send(TungsteniteMessage::Binary(bytes.into()))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
        }
    }

    /// Receive and deserialise the next [`ClientMessage`]. Returns `None` on
    /// clean close, deserialisation failure (after replying with an error),
    /// or transport error.
    pub async fn recv_msg(&mut self) -> Option<ClientMessage> {
        let bytes = match self.recv_bytes().await? {
            Ok(b) => b,
            Err(()) => return None,
        };
        match decode_client_message(&bytes) {
            Ok(msg) => {
                trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
                Some(msg)
            }
            Err(e) => {
                warn!(error = %e, "failed to deserialize client message");
                self.send_error(400, "malformed message".into()).await;
                None
            }
        }
    }

    /// Serialise and send a [`ServerMessage`].
    pub async fn send_msg(&mut self, msg: &ServerMessage) -> Result<(), ()> {
        let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
            warn!(error = %e, "failed to serialize server message");
        })?;
        trace!(?msg, bytes = bytes.len(), "send ServerMessage");
        self.send_bytes(bytes.to_vec()).await
    }

    pub async fn send_error(&mut self, code: u16, message: String) {
        let _ = self.send_msg(&ServerMessage::Error { code, message }).await;
    }

    pub async fn send_reject(&mut self, code: u16, reason: String) {
        let _ = self.send_msg(&ServerMessage::Reject { code, reason }).await;
    }

    /// Split the socket into a reader half and a cloneable writer half. The
    /// writer is backed by a bounded mpsc drained by a spawned task that owns
    /// the WebSocket sink. `send_chunk_timeout` bounds how long each producer
    /// `send` may wait when the queue is full — exceeding it indicates the
    /// peer's TCP receive side is stalled.
    pub fn split(self, send_chunk_timeout: Duration) -> (ProtoReader, ProtoWriter) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(WRITER_QUEUE_DEPTH);
        let writer = ProtoWriter {
            tx,
            send_chunk_timeout,
        };
        match self {
            Self::Axum(ws) => {
                let (sink, stream) = (*ws).split();
                tokio::spawn(axum_writer_task(rx, sink));
                (ProtoReader::Axum(stream), writer)
            }
            Self::Tungstenite(ws) => {
                let (sink, stream) = (*ws).split();
                tokio::spawn(tungstenite_writer_task(rx, sink));
                (ProtoReader::Tungstenite(stream), writer)
            }
        }
    }
}

// ── Read half ─────────────────────────────────────────────────────────────────

pub enum ProtoReader {
    Axum(SplitStream<WebSocket>),
    Tungstenite(SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl ProtoReader {
    pub async fn recv_msg(&mut self) -> Option<ClientMessage> {
        loop {
            let frame = match self {
                Self::Axum(s) => s.next().await,
                Self::Tungstenite(s) => match s.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => match decode_client_message(&bytes) {
                        Ok(msg) => {
                            trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
                            return Some(msg);
                        }
                        Err(e) => {
                            warn!(error = %e, "failed to deserialize client message");
                            return None;
                        }
                    },
                    Ok(TungsteniteMessage::Close(_)) => return None,
                    Ok(TungsteniteMessage::Ping(_) | TungsteniteMessage::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(error = %e, "WebSocket recv error");
                        return None;
                    }
                },
            };
            match frame? {
                Ok(AxumMessage::Binary(bytes)) => match decode_client_message(&bytes) {
                    Ok(msg) => {
                        trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
                        return Some(msg);
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize client message");
                        return None;
                    }
                },
                Ok(AxumMessage::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => {
                    debug!(error = %e, "WebSocket recv error");
                    return None;
                }
            }
        }
    }
}

// ── Write half ────────────────────────────────────────────────────────────────

/// Cloneable producer side of the post-split connection. Each send serialises
/// the message and pushes the bytes into a bounded mpsc; the writer task does
/// the actual WS write. Producer-observable back-pressure is bounded by
/// [`Self::send_chunk_timeout`]: queue full for longer than this is treated as
/// a peer stall and surfaced as an error.
#[derive(Clone)]
pub struct ProtoWriter {
    pub(crate) tx: mpsc::Sender<Vec<u8>>,
    pub(crate) send_chunk_timeout: Duration,
}

impl ProtoWriter {
    pub async fn send_msg(&self, msg: &ServerMessage) -> Result<(), ()> {
        let bytes = match rkyv::to_bytes::<RkyvError>(msg) {
            Ok(b) => b.to_vec(),
            Err(e) => {
                warn!(error = %e, "failed to serialize server message");
                return Err(());
            }
        };
        trace!(?msg, bytes = bytes.len(), "send ServerMessage");
        match tokio::time::timeout(self.send_chunk_timeout, self.tx.send(bytes)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(()),
            Err(_) => {
                warn!(
                    timeout_secs = self.send_chunk_timeout.as_secs(),
                    "WS writer queue full beyond send timeout — peer TCP stalled"
                );
                Err(())
            }
        }
    }
}

async fn axum_writer_task(
    mut rx: mpsc::Receiver<Vec<u8>>,
    mut sink: futures::stream::SplitSink<WebSocket, AxumMessage>,
) {
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = sink.send(AxumMessage::Binary(bytes.into())).await {
            debug!(error = %e, "axum WS writer task: send failed; exiting");
            break;
        }
    }
}

async fn tungstenite_writer_task(
    mut rx: mpsc::Receiver<Vec<u8>>,
    mut sink: futures::stream::SplitSink<
        WebSocketStream<MaybeTlsStream<TcpStream>>,
        TungsteniteMessage,
    >,
) {
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = sink.send(TungsteniteMessage::Binary(bytes.into())).await {
            debug!(error = %e, "tungstenite WS writer task: send failed; exiting");
            break;
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

pub async fn recv_client_msg(reader: &mut ProtoReader) -> Option<ClientMessage> {
    reader.recv_msg().await
}

pub async fn send_server_msg(writer: &ProtoWriter, msg: &ServerMessage) -> Result<(), ()> {
    writer.send_msg(msg).await
}

pub async fn send_error(writer: &ProtoWriter, code: u16, message: String) {
    let _ = writer
        .send_msg(&ServerMessage::Error { code, message })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_message_size_is_eight_mib() {
        assert_eq!(MAX_PROTO_MESSAGE_SIZE, 8 * 1024 * 1024);
    }

    #[test]
    fn nar_push_chunk_size_is_four_mib() {
        assert_eq!(NAR_PUSH_CHUNK_SIZE, 4 * 1024 * 1024);
    }
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// Construct a writer that's not backed by a draining task, so we can
    /// observe queue-full back-pressure deterministically.
    fn unwired_writer(
        capacity: usize,
        timeout: Duration,
    ) -> (ProtoWriter, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(capacity);
        (
            ProtoWriter {
                tx,
                send_chunk_timeout: timeout,
            },
            rx,
        )
    }

    /// A backed-up writer queue must surface as `Err(())` from `send_msg`
    /// after the configured timeout — never hang. This is what makes a
    /// stalled peer detectable on the server side instead of waiting for
    /// the worker's 600 s receive ceiling.
    #[tokio::test(start_paused = true)]
    async fn send_msg_times_out_when_queue_is_full() {
        let (writer, _rx) = unwired_writer(1, Duration::from_secs(5));
        writer.tx.send(vec![1, 2, 3]).await.unwrap();

        let msg = ServerMessage::Reject {
            code: 400,
            reason: "stalled".into(),
        };
        let res = writer.send_msg(&msg).await;
        assert!(
            res.is_err(),
            "send_msg must report failure when the writer queue stays full past send_chunk_timeout",
        );
    }

    /// Fast-path: when the queue has room, send_msg returns Ok immediately
    /// (the drainer task isn't required for correctness here — the mpsc
    /// receiver just keeps the channel open).
    #[tokio::test]
    async fn send_msg_succeeds_when_queue_has_room() {
        let (writer, mut rx) = unwired_writer(2, Duration::from_secs(5));
        let msg = ServerMessage::Reject {
            code: 200,
            reason: "ok".into(),
        };
        writer.send_msg(&msg).await.expect("queue had room");
        let bytes = rx.try_recv().expect("byte buffer enqueued");
        assert!(!bytes.is_empty(), "serialised message should be non-empty");
    }
}
