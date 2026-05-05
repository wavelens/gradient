/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Low-level WebSocket I/O.
//!
//! After the handshake completes, the connection is split into a single-owner
//! [`ProtoReader`] (used by the dispatch loop) and a cloneable [`ProtoWriter`]
//! (mpsc-backed, drained by a spawned writer task). Splitting decouples reads
//! from writes so a slow outbound NAR transfer cannot block inbound message
//! handling, and lets concurrent NAR-serving tasks share the wire safely.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use gradient_core::types::ids::OrganizationId;
use gradient_core::types::*;
use rkyv::rancor::Error as RkyvError;
use sea_orm::EntityTrait;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, tungstenite::Message as TungsteniteMessage,
};
use tracing::{debug, error, trace, warn};

use crate::messages::{ClientMessage, ServerMessage};
use scheduler::Scheduler;

// ── Constants ─────────────────────────────────────────────────────────────────

pub(super) const JOB_OFFER_CHUNK_SIZE: usize = 1_000;
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
pub(crate) enum ProtoSocket {
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
    pub(super) async fn recv_msg(&mut self) -> Option<ClientMessage> {
        let bytes = match self.recv_bytes().await? {
            Ok(b) => b,
            Err(()) => return None,
        };
        match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
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
    pub(super) async fn send_msg(&mut self, msg: &ServerMessage) -> Result<(), ()> {
        let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
            warn!(error = %e, "failed to serialize server message");
        })?;
        trace!(?msg, bytes = bytes.len(), "send ServerMessage");
        self.send_bytes(bytes.to_vec()).await
    }

    pub(super) async fn send_error(&mut self, code: u16, message: String) {
        let _ = self.send_msg(&ServerMessage::Error { code, message }).await;
    }

    pub(super) async fn send_reject(&mut self, code: u16, reason: String) {
        let _ = self.send_msg(&ServerMessage::Reject { code, reason }).await;
    }

    /// Split the socket into a reader half and a cloneable writer half. The
    /// writer is backed by a bounded mpsc drained by a spawned task that owns
    /// the WebSocket sink. `send_chunk_timeout` bounds how long each producer
    /// `send` may wait when the queue is full — exceeding it indicates the
    /// peer's TCP receive side is stalled.
    pub(super) fn split(self, send_chunk_timeout: Duration) -> (ProtoReader, ProtoWriter) {
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

pub(crate) enum ProtoReader {
    Axum(SplitStream<WebSocket>),
    Tungstenite(SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl ProtoReader {
    pub(super) async fn recv_msg(&mut self) -> Option<ClientMessage> {
        loop {
            let frame = match self {
                Self::Axum(s) => s.next().await,
                Self::Tungstenite(s) => match s.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => {
                        match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
                            Ok(msg) => {
                                trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
                                return Some(msg);
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize client message");
                                return None;
                            }
                        }
                    }
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
                Ok(AxumMessage::Binary(bytes)) => {
                    match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
                        Ok(msg) => {
                            trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
                            return Some(msg);
                        }
                        Err(e) => {
                            warn!(error = %e, "failed to deserialize client message");
                            return None;
                        }
                    }
                }
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
pub(crate) struct ProtoWriter {
    tx: mpsc::Sender<Vec<u8>>,
    send_chunk_timeout: Duration,
}

impl ProtoWriter {
    pub(super) async fn send_msg(&self, msg: &ServerMessage) -> Result<(), ()> {
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

// ── Free helpers (post-split) ─────────────────────────────────────────────────

pub(super) async fn recv_client_msg(reader: &mut ProtoReader) -> Option<ClientMessage> {
    reader.recv_msg().await
}

pub(super) async fn send_server_msg(writer: &ProtoWriter, msg: &ServerMessage) -> Result<(), ()> {
    writer.send_msg(msg).await
}

pub(super) async fn send_error(writer: &ProtoWriter, code: u16, message: String) {
    let _ = writer
        .send_msg(&ServerMessage::Error { code, message })
        .await;
}

/// Push any pending job candidates to the worker (delta).
pub(super) async fn push_pending_candidates(
    writer: &ProtoWriter,
    scheduler: &Scheduler,
    peer_id: &str,
) {
    let candidates = scheduler.get_new_job_candidates(peer_id).await;
    if candidates.is_empty() {
        return;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta) after message processing");
    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        let _ = send_server_msg(
            writer,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await;
    }
}

// ── NAR streaming ─────────────────────────────────────────────────────────────

/// Stream a single requested NAR from `nar_storage` to the worker.
///
/// Hardening notes:
/// - The initial storage open is wrapped in `storage_open_timeout`. A stalled
///   backend (e.g. S3 hung TCP) used to silently consume the dispatch loop's
///   600 s waiter ceiling; now it surfaces as a `NarUnavailable` within the
///   open timeout.
/// - The chunked send path uses [`ProtoWriter`], which bounds per-chunk send
///   waits via the queue + `send_chunk_timeout` configured at split time.
///   A stalled peer is detected as `Err(())` from `send_server_msg` and
///   triggers a best-effort `NarAbort`.
/// - The body is read from `object_store`'s streaming API — no full file is
///   ever held in memory. Chunks are coalesced/split to `NAR_PUSH_CHUNK_SIZE`.
/// - Per-chunk read from the storage stream is also bounded so a backend that
///   sends the first byte and then hangs cannot pin the task indefinitely.
pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
) -> anyhow::Result<()> {
    let proto_cfg = &state.config.proto;
    let storage_open_timeout = Duration::from_secs(proto_cfg.nar_storage_open_timeout_secs);
    let chunk_read_timeout = Duration::from_secs(proto_cfg.nar_send_chunk_timeout_secs);

    let Some(hash) = store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
    else {
        let reason = format!("invalid store path: {store_path}");
        send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
        return Err(anyhow::anyhow!(reason));
    };

    let opened =
        tokio::time::timeout(storage_open_timeout, state.nar_storage.get_stream(hash)).await;
    let mut stream = match opened {
        Ok(Ok(Some((_size, s)))) => s,
        Ok(Ok(None)) => {
            let reason = format!("NAR not found in cache for {store_path}");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
        Ok(Err(e)) => {
            let reason = format!("nar_storage.get_stream({hash}) failed: {e}");
            error!(%store_path, error = %e, "NAR storage read error");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
        Err(_) => {
            let reason = format!(
                "nar_storage.get_stream({hash}) timed out after {}s",
                storage_open_timeout.as_secs()
            );
            warn!(%store_path, "NAR storage open timed out");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
    };

    // Coalesce arbitrary-sized backend chunks into NAR_PUSH_CHUNK_SIZE-sized
    // frames so the wire format stays predictable and the receiver's per-frame
    // budget (MAX_PROTO_MESSAGE_SIZE) is never threatened by upstream variance.
    let mut buf: Vec<u8> = Vec::with_capacity(NAR_PUSH_CHUNK_SIZE);
    let mut offset: u64 = 0;
    let mut total: u64 = 0;
    let mut chunks_sent: u64 = 0;

    loop {
        let next = tokio::time::timeout(chunk_read_timeout, stream.next()).await;
        let item = match next {
            Ok(Some(x)) => x,
            Ok(None) => break, // end of stream
            Err(_) => {
                let reason = format!(
                    "NAR storage read stalled > {}s mid-transfer",
                    chunk_read_timeout.as_secs()
                );
                warn!(%store_path, "NAR storage read stall");
                let _ = send_server_msg(
                    writer,
                    &ServerMessage::NarAbort {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        reason: reason.clone(),
                    },
                )
                .await;
                return Err(anyhow::anyhow!(reason));
            }
        };
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("NAR storage stream error: {e}");
                error!(%store_path, error = %e, "NAR storage stream error");
                let _ = send_server_msg(
                    writer,
                    &ServerMessage::NarAbort {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        reason: reason.clone(),
                    },
                )
                .await;
                return Err(anyhow::anyhow!(reason));
            }
        };

        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let want = NAR_PUSH_CHUNK_SIZE - buf.len();
            let take = slice.len().min(want);
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == NAR_PUSH_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(NAR_PUSH_CHUNK_SIZE));
                let chunk_len = chunk.len() as u64;
                if send_server_msg(
                    writer,
                    &ServerMessage::NarPush {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        data: chunk,
                        offset,
                        is_final: false,
                    },
                )
                .await
                .is_err()
                {
                    let reason = format!("WebSocket send stalled mid-NarPush at offset {offset}");
                    let _ = send_server_msg(
                        writer,
                        &ServerMessage::NarAbort {
                            job_id: job_id.to_owned(),
                            store_path: store_path.to_owned(),
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    return Err(anyhow::anyhow!(reason));
                }
                offset += chunk_len;
                total += chunk_len;
                chunks_sent += 1;
            }
        }
    }

    // Flush whatever's left (possibly empty) as the `is_final` frame.
    let final_len = buf.len() as u64;
    if send_server_msg(
        writer,
        &ServerMessage::NarPush {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            data: buf,
            offset,
            is_final: true,
        },
    )
    .await
    .is_err()
    {
        let reason = format!("WebSocket send stalled on final NarPush at offset {offset}");
        let _ = send_server_msg(
            writer,
            &ServerMessage::NarAbort {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                reason: reason.clone(),
            },
        )
        .await;
        return Err(anyhow::anyhow!(reason));
    }
    total += final_len;
    chunks_sent += 1;

    debug!(%store_path, bytes = total, chunks = chunks_sent, "NarRequest served (streaming)");
    Ok(())
}

async fn send_nar_unavailable(
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    reason: String,
) {
    let _ = send_server_msg(
        writer,
        &ServerMessage::NarUnavailable {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            reason,
        },
    )
    .await;
}

// ── Credential delivery ───────────────────────────────────────────────────────

pub(super) async fn send_credentials_for_job(
    writer: &ProtoWriter,
    state: &ServerState,
    scheduler: &scheduler::Scheduler,
    worker_id: &str,
    job: &gradient_core::types::proto::Job,
    org_id: OrganizationId,
) {
    use gradient_core::types::proto::{FlakeTask, Job};

    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let worker_can_fetch = caps.as_ref().map(|c| c.fetch).unwrap_or(false);

    match job {
        Job::Flake(flake_job) => {
            if worker_can_fetch && flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                send_ssh_key_credential(writer, state, org_id).await;
            }
        }
        Job::Build(_) => {}
    }
}

async fn send_ssh_key_credential(
    writer: &ProtoWriter,
    state: &ServerState,
    org_id: OrganizationId,
) {
    use gradient_core::types::proto::CredentialKind;

    match EOrganization::find_by_id(org_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(org)) => {
            match gradient_core::sources::ssh_key::decrypt_ssh_private_key(
                &state.config.secrets.crypt_secret_file,
                org,
                &state.config.server.serve_url,
            ) {
                Ok((private_key, _public_key)) => {
                    let _ = send_server_msg(
                        writer,
                        &ServerMessage::Credential {
                            kind: CredentialKind::SshKey,
                            data: private_key.into_bytes(),
                        },
                    )
                    .await;
                    debug!(%org_id, "SSH key credential sent");
                }
                Err(e) => {
                    debug!(%org_id, error = %e, "no SSH key for org (may be HTTPS repo)");
                }
            }
        }
        Ok(None) => warn!(%org_id, "org not found for SSH key lookup"),
        Err(e) => warn!(%org_id, error = %e, "failed to fetch org for SSH key"),
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
        // Fill the single slot directly via the inner tx (drainer never runs).
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

#[cfg(test)]
mod serve_nar_tests {
    use super::*;
    use rkyv::rancor::Error as RkyvError;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use test_support::state::test_state;
    use tokio::sync::mpsc;

    /// Spy writer: records every message the server attempted to send so the
    /// test can assert exactly which protocol frames were emitted (NarPush,
    /// NarUnavailable, NarAbort, …).
    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        (
            ProtoWriter {
                tx,
                send_chunk_timeout: timeout,
            },
            rx,
        )
    }

    fn decode(bytes: &[u8]) -> ServerMessage {
        rkyv::from_bytes::<ServerMessage, RkyvError>(bytes).expect("decode ServerMessage")
    }

    fn variant_of(msg: &ServerMessage) -> &'static str {
        match msg {
            ServerMessage::NarPush { .. } => "NarPush",
            ServerMessage::NarUnavailable { .. } => "NarUnavailable",
            ServerMessage::NarAbort { .. } => "NarAbort",
            _ => "other",
        }
    }

    /// Streamed payload arrives as one or more `NarPush` frames whose
    /// concatenated `data` matches the original bytes, with the final frame
    /// flagged `is_final=true`.
    #[tokio::test]
    async fn serve_streams_full_payload_in_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        // 9 MiB of distinguishable bytes — straddles the 4 MiB chunk boundary
        // so the stream must produce at least 3 NarPush frames.
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        // Hash-name format expected by NarStore: 32-char base32 + suffix.
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state.nar_storage.put(hash, payload.clone()).await.unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path)
            .await
            .expect("serve must succeed");

        let mut assembled = Vec::with_capacity(payload.len());
        let mut frames = 0u32;
        let mut saw_final = false;
        while let Ok(bytes) = rx.try_recv() {
            let msg = decode(&bytes);
            assert_eq!(variant_of(&msg), "NarPush", "only NarPush frames expected");
            if let ServerMessage::NarPush { data, is_final, .. } = msg {
                assembled.extend_from_slice(&data);
                if is_final {
                    saw_final = true;
                }
            }
            frames += 1;
        }
        assert!(
            frames >= 3,
            "9 MiB / 4 MiB chunks → at least 3 frames, got {frames}"
        );
        assert!(saw_final, "the last frame must be is_final=true");
        assert_eq!(
            assembled, payload,
            "concatenated NarPush data must equal source"
        );
    }

    /// Missing object → `NarUnavailable` (not `NarAbort`, no NarPush) and an
    /// `Err` from `serve_nar_request`.
    #[tokio::test]
    async fn serve_emits_nar_unavailable_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &state,
            &writer,
            "job-1",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-missing",
        )
        .await;
        assert!(res.is_err(), "missing path must surface as Err");

        let bytes = rx.try_recv().expect("expect one frame");
        let msg = decode(&bytes);
        assert_eq!(variant_of(&msg), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }
}
