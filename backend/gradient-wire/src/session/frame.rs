/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame as AxumCloseFrame, Message as AxumMessage, WebSocket};
use bytes::Bytes;
use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, trace, warn};

use gradient_util::shutdown::Shutdown;
use gradient_util::telemetry::{GAUGES, STATS, fill_permille, metric};

use crate::codec::agreement::{agree, version_frame};
use crate::codec::{self, Proto};
use crate::messages::{ClientMessage, PROTO_VERSIONS, ServerMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("message failed to encode")]
    Encode,
    #[error("WebSocket closed")]
    Closed,
    #[error("WS writer queue full beyond send timeout - peer TCP stalled")]
    Stalled,
}

type WriterTask = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

pub const JOB_OFFER_CHUNK_SIZE: usize = 1_000;
pub use crate::constants::BULK_CHUNK_SIZE;

pub const MAX_PROTO_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

/// [`MAX_PROTO_MESSAGE_SIZE`] is bounding memory, not liveness. A message far larger than the send
/// buffer is leaving its sender blocked mid-write. Two peers blocked mid-write never return to
/// reading, and the connection is dying on a send timeout. One-way bulk transfers like `NarPush`
/// are exempt.
pub const SAFE_INFLIGHT_MESSAGE_SIZE: usize = 2 * 1024 * 1024;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

const WRITER_QUEUE_DEPTH: usize = 16;

const CONTROL_QUEUE_DEPTH: usize = 256;

const WRITE_BATCH: usize = 32;

const BULK_BATCH_BYTES: usize = 256 * 1024;

pub trait WireMessage: Proto + std::fmt::Debug + Send + 'static {
    fn variant_name(&self) -> &'static str;

    fn carries_secret(&self) -> bool;

    fn is_bulk(&self) -> bool;
}

pub struct Redacted<'a, M>(pub &'a M);

impl<M: WireMessage> std::fmt::Debug for Redacted<'_, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.carries_secret() {
            return write!(f, "{} {{ .. }}", self.0.variant_name());
        }

        std::fmt::Debug::fmt(self.0, f)
    }
}

pub enum FirstMessage {
    Worker(ClientMessage),
    Server(ServerMessage),
}

fn first_message(bytes: Bytes, version: u16) -> Option<FirstMessage> {
    if let Ok(greeting @ ClientMessage::InitConnection { .. }) =
        codec::from_bytes(bytes.clone(), version)
    {
        return Some(FirstMessage::Worker(greeting));
    }

    match codec::from_bytes(bytes, version) {
        Ok(authenticate @ ServerMessage::Authenticate { .. }) => {
            Some(FirstMessage::Server(authenticate))
        }
        _ => None,
    }
}

fn encode<M: WireMessage>(msg: &M, version: u16) -> Result<Bytes, SendError> {
    codec::to_bytes(msg, version).map_err(|e| {
        warn!(error = %e, "failed to encode {}", msg.variant_name());
        SendError::Encode
    })
}

impl WireMessage for ClientMessage {
    fn variant_name(&self) -> &'static str {
        ClientMessage::variant_name(self)
    }

    fn carries_secret(&self) -> bool {
        ClientMessage::carries_secret(self)
    }

    fn is_bulk(&self) -> bool {
        matches!(
            self,
            ClientMessage::UploadChunk { .. }
                | ClientMessage::NarRequestResume { .. }
                | ClientMessage::LogChunk { .. }
                | ClientMessage::JobCompleted { .. }
        )
    }
}

impl WireMessage for ServerMessage {
    fn variant_name(&self) -> &'static str {
        ServerMessage::variant_name(self)
    }

    fn carries_secret(&self) -> bool {
        ServerMessage::carries_secret(self)
    }

    fn is_bulk(&self) -> bool {
        matches!(
            self,
            ServerMessage::NarPush { .. }
                | ServerMessage::NarStreamHeader { .. }
                | ServerMessage::NarUnavailable { .. }
                | ServerMessage::NarAbort { .. }
                | ServerMessage::EvalCachePullResult { .. }
                | ServerMessage::EvalCacheChunk { .. }
        )
    }
}

pub struct ProtoSocket {
    transport: Transport,
    version: Option<u16>,
}

enum Transport {
    Axum(Box<WebSocket>),
    Tungstenite(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

const PROTOCOL_ERROR: u16 = 1002;

fn log_close_reason(reason: &str) {
    if !reason.is_empty() {
        warn!(%reason, "peer closed /proto");
    }
}

impl Transport {
    async fn recv_bytes(&mut self) -> Option<Bytes> {
        match self {
            Self::Axum(ws) => loop {
                match ws.recv().await? {
                    Ok(AxumMessage::Binary(bytes)) => return Some(bytes),
                    Ok(AxumMessage::Close(frame)) => {
                        log_close_reason(frame.as_ref().map_or("", |f| f.reason.as_str()));
                        return None;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        warn!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
            Self::Tungstenite(ws) => loop {
                match ws.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => return Some(bytes),
                    Ok(TungsteniteMessage::Close(frame)) => {
                        log_close_reason(frame.as_ref().map_or("", |f| f.reason.as_str()));
                        return None;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        warn!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
        }
    }

    async fn send_bytes(&mut self, bytes: Bytes) -> Result<(), SendError> {
        match self {
            Self::Axum(ws) => ws
                .send(AxumMessage::Binary(bytes))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
            Self::Tungstenite(ws) => ws
                .send(TungsteniteMessage::Binary(bytes))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
        }
        .map_err(|()| SendError::Closed)
    }

    async fn close(&mut self, reason: String) {
        let sent = match self {
            Self::Axum(ws) => ws
                .send(AxumMessage::Close(Some(AxumCloseFrame {
                    code: PROTOCOL_ERROR,
                    reason: reason.into(),
                })))
                .await
                .map_err(|e| e.to_string()),
            Self::Tungstenite(ws) => ws
                .send(TungsteniteMessage::Close(Some(TungsteniteCloseFrame {
                    code: CloseCode::from(PROTOCOL_ERROR),
                    reason: reason.into(),
                })))
                .await
                .map_err(|e| e.to_string()),
        };
        if let Err(error) = sent {
            debug!(%error, "WebSocket close failed");
        }
    }
}

impl ProtoSocket {
    pub fn axum(ws: WebSocket) -> Self {
        Self {
            transport: Transport::Axum(Box::new(ws)),
            version: None,
        }
    }

    pub fn tungstenite(ws: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self {
            transport: Transport::Tungstenite(Box::new(ws)),
            version: None,
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn with_version(mut self, version: u16) -> Self {
        self.version = Some(version);
        self
    }

    pub fn version(&self) -> Option<u16> {
        self.version
    }

    #[cfg(test)]
    pub(crate) fn tungstenite_stream(&self) -> Option<&WebSocketStream<MaybeTlsStream<TcpStream>>> {
        match &self.transport {
            Transport::Tungstenite(ws) => Some(ws),
            Transport::Axum(_) => None,
        }
    }

    pub async fn agree_version(&mut self) -> Option<u16> {
        if self.version.is_none() {
            self.version = self.agree().await;
        }

        self.version
    }

    async fn agree(&mut self) -> Option<u16> {
        self.transport
            .send_bytes(version_frame(&PROTO_VERSIONS))
            .await
            .ok()?;

        let frame = self.transport.recv_bytes().await?;
        match agree(&PROTO_VERSIONS, &frame) {
            Ok(version) => {
                debug!(version, "agreed on the /proto version");
                Some(version)
            }
            Err(reason) => {
                warn!(%reason, "closing /proto connection");
                self.transport.close(reason).await;
                None
            }
        }
    }

    async fn recv_bytes(&mut self) -> Option<(Bytes, u16)> {
        let version = self.agree_version().await?;
        Some((self.transport.recv_bytes().await?, version))
    }

    async fn send<M: WireMessage>(&mut self, msg: &M) -> Result<(), SendError> {
        let version = self.agree_version().await.ok_or(SendError::Closed)?;
        let bytes = encode(msg, version)?;
        trace!(msg = ?Redacted(msg), bytes = bytes.len(), "send message");
        self.transport.send_bytes(bytes).await
    }

    pub async fn recv_server_msg(&mut self) -> Option<ServerMessage> {
        let (bytes, version) = self.recv_bytes().await?;
        let len = bytes.len();
        match codec::from_bytes::<ServerMessage>(bytes, version) {
            Ok(msg) => {
                trace!(msg = ?Redacted(&msg), bytes = len, "recv ServerMessage");
                Some(msg)
            }
            Err(e) => {
                warn!(error = %e, "failed to decode server message");
                None
            }
        }
    }

    pub async fn send_client_msg(&mut self, msg: &ClientMessage) -> Result<(), SendError> {
        self.send(msg).await
    }

    pub async fn recv_first_message(&mut self) -> Option<FirstMessage> {
        let (bytes, version) = self.recv_bytes().await?;
        first_message(bytes, version)
    }

    pub async fn recv_msg(&mut self) -> Option<ClientMessage> {
        let (bytes, version) = self.recv_bytes().await?;
        let len = bytes.len();
        match codec::from_bytes::<ClientMessage>(bytes, version) {
            Ok(msg) => {
                trace!(msg = ?Redacted(&msg), bytes = len, "recv ClientMessage");
                Some(msg)
            }
            Err(e) => {
                warn!(error = %e, "failed to decode client message");
                self.send_error(400, "malformed message".into()).await;
                None
            }
        }
    }

    pub async fn send_msg(&mut self, msg: &ServerMessage) -> Result<(), SendError> {
        self.send(msg).await
    }

    pub async fn send_error(&mut self, code: u16, message: String) {
        let _ = self.send_msg(&ServerMessage::Error { code, message }).await;
    }

    pub async fn send_reject(&mut self, code: u16, reason: String) {
        let _ = self.send_msg(&ServerMessage::Reject { code, reason }).await;
    }

    pub fn split(
        self,
        send_chunk_timeout: Duration,
        shutdown: &Shutdown,
    ) -> (ProtoReader, ProtoWriter) {
        self.split_typed(send_chunk_timeout, |task| {
            shutdown.spawn(task);
        })
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "no shutdown tracker on the peer side"
    )]
    pub fn split_peer(
        self,
        send_chunk_timeout: Duration,
    ) -> (ServerReader, ClientWriter, JoinHandle<()>) {
        let mut writer_task = None;
        let (reader, writer) = self.split_typed(send_chunk_timeout, |task| {
            writer_task = Some(tokio::spawn(task));
        });
        (
            reader,
            writer,
            writer_task.expect("split_typed always spawns the writer task"),
        )
    }

    fn split_typed<In: WireMessage, Out: WireMessage>(
        self,
        send_chunk_timeout: Duration,
        spawn: impl FnOnce(WriterTask),
    ) -> (MsgReader<In>, MsgWriter<Out>) {
        let version = self
            .version
            .expect("the handshake agreed a protocol version before the split");

        let (tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (control_tx, control_rx) = mpsc::channel::<Bytes>(CONTROL_QUEUE_DEPTH);
        let writer = MsgWriter {
            tx,
            control_tx,
            send_chunk_timeout,
            version,
            observer: None,
            _direction: PhantomData,
        };
        let lanes = WriterLanes::new(control_rx, bulk_rx);
        let inner = match self.transport {
            Transport::Axum(ws) => {
                let (sink, stream) = (*ws).split();
                spawn(Box::pin(axum_writer_task(lanes, sink)));
                ReaderInner::Axum(stream)
            }
            Transport::Tungstenite(ws) => {
                let (sink, stream) = (*ws).split();
                spawn(Box::pin(tungstenite_writer_task(lanes, sink)));
                ReaderInner::Tungstenite(stream)
            }
        };
        (
            MsgReader {
                inner,
                version,
                observer: None,
            },
            writer,
        )
    }
}

enum ReaderInner {
    Axum(SplitStream<WebSocket>),
    Tungstenite(SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl ReaderInner {
    async fn recv_bytes(&mut self) -> Option<Bytes> {
        loop {
            match self {
                Self::Axum(s) => match s.next().await? {
                    Ok(AxumMessage::Binary(bytes)) => return Some(bytes),
                    Ok(AxumMessage::Close(frame)) => {
                        log_close_reason(frame.as_ref().map_or("", |f| f.reason.as_str()));
                        return None;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        warn!(error = %e, "WebSocket recv error");
                        return None;
                    }
                },
                Self::Tungstenite(s) => match s.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => return Some(bytes),
                    Ok(TungsteniteMessage::Close(frame)) => {
                        log_close_reason(frame.as_ref().map_or("", |f| f.reason.as_str()));
                        return None;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        warn!(error = %e, "WebSocket recv error");
                        return None;
                    }
                },
            }
        }
    }
}

pub struct MsgReader<M> {
    inner: ReaderInner,
    version: u16,
    observer: Option<Arc<dyn MsgObserver<M>>>,
}

pub type ProtoReader = MsgReader<ClientMessage>;
pub type ServerReader = MsgReader<ServerMessage>;

impl<M> MsgReader<M> {
    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn with_observer(mut self, observer: Arc<dyn MsgObserver<M>>) -> Self {
        self.observer = Some(observer);
        self
    }
}

impl<M: WireMessage> MsgReader<M> {
    pub async fn recv(&mut self) -> Option<M> {
        let bytes = self.inner.recv_bytes().await?;
        let len = bytes.len();
        let msg = codec::from_bytes::<M>(bytes, self.version)
            .map_err(|e| warn!(error = %e, "failed to decode message"))
            .ok()?;
        trace!(variant = msg.variant_name(), bytes = len, "recv message");
        if let Some(observer) = &self.observer {
            observer.observe(&msg, len);
        }

        Some(msg)
    }
}

pub trait MsgObserver<M>: Send + Sync {
    fn observe(&self, msg: &M, len: usize);
}

pub struct MsgWriter<M> {
    pub(crate) tx: mpsc::Sender<Bytes>,
    pub(crate) control_tx: mpsc::Sender<Bytes>,
    pub(crate) send_chunk_timeout: Duration,
    pub(crate) version: u16,
    pub(crate) observer: Option<Arc<dyn MsgObserver<M>>>,
    pub(crate) _direction: PhantomData<M>,
}

pub type ProtoWriter = MsgWriter<ServerMessage>;
pub type ClientWriter = MsgWriter<ClientMessage>;

impl<M> Clone for MsgWriter<M> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            control_tx: self.control_tx.clone(),
            send_chunk_timeout: self.send_chunk_timeout,
            version: self.version,
            observer: self.observer.clone(),
            _direction: PhantomData,
        }
    }
}

impl<M> MsgWriter<M> {
    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn with_observer(mut self, observer: Arc<dyn MsgObserver<M>>) -> Self {
        self.observer = Some(observer);
        self
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn spy(send_chunk_timeout: Duration) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(64);
        let writer = Self {
            control_tx: tx.clone(),
            tx,
            send_chunk_timeout,
            version: *PROTO_VERSIONS.end(),
            observer: None,
            _direction: PhantomData,
        };
        (writer, rx)
    }
}

impl<M: WireMessage> MsgWriter<M> {
    pub async fn send_msg(&self, msg: &M) -> Result<(), SendError> {
        let bytes = encode(msg, self.version)?;
        trace!(msg = ?Redacted(msg), bytes = bytes.len(), "send message");
        if let Some(observer) = &self.observer {
            observer.observe(msg, bytes.len());
        }

        let bulk = msg.is_bulk();
        let lane = if bulk { &self.tx } else { &self.control_tx };
        let lane_name = if bulk { "bulk" } else { "control" };

        match tokio::time::timeout(self.send_chunk_timeout, lane.send(bytes)).await {
            Ok(Ok(())) => {
                let peak = if bulk {
                    &GAUGES.bulk_lane_peak
                } else {
                    &GAUGES.control_lane_peak
                };
                peak.observe(fill_permille(lane.capacity(), lane.max_capacity()));

                Ok(())
            }
            Ok(Err(_)) => Err(SendError::Closed),
            Err(_) => {
                STATS.record(metric::PROTO_SEND_STALLS, lane_name, 1.0);
                warn!(
                    timeout_secs = self.send_chunk_timeout.as_secs(),
                    bulk,
                    "{}",
                    SendError::Stalled
                );
                Err(SendError::Stalled)
            }
        }
    }
}

pub(crate) struct WriterLanes {
    control: mpsc::Receiver<Bytes>,
    bulk: mpsc::Receiver<Bytes>,
    control_open: bool,
    bulk_open: bool,
}

impl WriterLanes {
    pub(crate) fn new(control: mpsc::Receiver<Bytes>, bulk: mpsc::Receiver<Bytes>) -> Self {
        Self {
            control,
            bulk,
            control_open: true,
            bulk_open: true,
        }
    }

    /// A batch is never mixing lanes. The writer task is flushing only after feeding the whole
    /// batch. A bulk chunk blocked mid-feed would strand a reply queued in front of it.
    pub(crate) async fn next_batch(&mut self, batch: &mut Vec<Bytes>) -> bool {
        loop {
            if !self.control_open && !self.bulk_open {
                return false;
            }
            for lane in [Lane::Control, Lane::Bulk] {
                self.drain_ready(lane, batch);
                if !batch.is_empty() {
                    return true;
                }
            }
            if !self.control_open && !self.bulk_open {
                return false;
            }
            let (lane, woke) = tokio::select! {
                biased;
                m = self.control.recv(), if self.control_open => (Lane::Control, m),
                m = self.bulk.recv(), if self.bulk_open => (Lane::Bulk, m),
            };
            match woke {
                Some(bytes) => {
                    batch.push(bytes);
                    self.drain_ready(lane, batch);
                    return true;
                }
                None => self.close(lane),
            }
        }
    }

    fn drain_ready(&mut self, lane: Lane, batch: &mut Vec<Bytes>) {
        if !self.is_open(lane) {
            return;
        }
        let rx = match lane {
            Lane::Control => &mut self.control,
            Lane::Bulk => &mut self.bulk,
        };
        let mut bytes: usize = batch.iter().map(Bytes::len).sum();
        let mut disconnected = false;
        while !batch_full(lane, batch.len(), bytes) {
            match rx.try_recv() {
                Ok(msg) => {
                    bytes += msg.len();
                    batch.push(msg);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.close(lane);
        }
    }

    fn is_open(&self, lane: Lane) -> bool {
        match lane {
            Lane::Control => self.control_open,
            Lane::Bulk => self.bulk_open,
        }
    }

    fn close(&mut self, lane: Lane) {
        match lane {
            Lane::Control => self.control_open = false,
            Lane::Bulk => self.bulk_open = false,
        }
    }
}

fn batch_full(lane: Lane, count: usize, bytes: usize) -> bool {
    match lane {
        Lane::Control => count >= WRITE_BATCH,
        Lane::Bulk => count >= WRITE_BATCH || bytes >= BULK_BATCH_BYTES,
    }
}

#[derive(Clone, Copy)]
enum Lane {
    Control,
    Bulk,
}

async fn axum_writer_task(
    mut lanes: WriterLanes,
    mut sink: futures::stream::SplitSink<WebSocket, AxumMessage>,
) {
    let mut batch = Vec::with_capacity(WRITE_BATCH);
    loop {
        if !lanes.next_batch(&mut batch).await {
            break;
        }
        for bytes in batch.drain(..) {
            if let Err(e) = sink.feed(AxumMessage::Binary(bytes)).await {
                debug!(error = %e, "axum WS writer task: send failed; exiting");
                return;
            }
        }
        if let Err(e) = sink.flush().await {
            debug!(error = %e, "axum WS writer task: flush failed; exiting");
            return;
        }
    }
}

async fn tungstenite_writer_task(
    mut lanes: WriterLanes,
    mut sink: futures::stream::SplitSink<
        WebSocketStream<MaybeTlsStream<TcpStream>>,
        TungsteniteMessage,
    >,
) {
    let mut batch = Vec::with_capacity(WRITE_BATCH);
    loop {
        if !lanes.next_batch(&mut batch).await {
            break;
        }
        for bytes in batch.drain(..) {
            if let Err(e) = sink.feed(TungsteniteMessage::Binary(bytes)).await {
                debug!(error = %e, "tungstenite WS writer task: send failed; exiting");
                return;
            }
        }
        if let Err(e) = sink.flush().await {
            debug!(error = %e, "tungstenite WS writer task: flush failed; exiting");
            return;
        }
    }
}

pub async fn recv_client_msg(reader: &mut ProtoReader) -> Option<ClientMessage> {
    reader.recv().await
}

pub async fn send_server_msg(writer: &ProtoWriter, msg: &ServerMessage) -> Result<(), SendError> {
    writer.send_msg(msg).await
}

pub async fn send_error(writer: &ProtoWriter, code: u16, message: String) {
    let _ = writer
        .send_msg(&ServerMessage::Error { code, message })
        .await;
}

pub async fn recv_server_msg(socket: &mut ProtoSocket) -> anyhow::Result<ServerMessage> {
    socket
        .recv_server_msg()
        .await
        .ok_or_else(|| anyhow::anyhow!("connection closed before next ServerMessage"))
}

pub async fn send_client_msg(socket: &mut ProtoSocket, msg: &ClientMessage) -> anyhow::Result<()> {
    socket
        .send_client_msg(msg)
        .await
        .map_err(|_| anyhow::anyhow!("failed to send ClientMessage"))
}

pub fn accept_tungstenite(
    ws: tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> ProtoSocket {
    ProtoSocket::tungstenite(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(bytes: usize) -> Bytes {
        Bytes::from(vec![0xbb; bytes])
    }

    #[tokio::test]
    async fn a_bulk_batch_stops_at_the_byte_cap() {
        let (bulk_tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (prio_tx, prio_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let mut lanes = WriterLanes::new(prio_rx, bulk_rx);
        for _ in 0..8 {
            bulk_tx.send(bulk(100 * 1024)).await.unwrap();
        }
        prio_tx.send(Bytes::from_static(&[0xff])).await.unwrap();

        let mut batch = Vec::new();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(batch, vec![Bytes::from_static(&[0xff])]);

        batch.clear();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(
            batch.len(),
            3,
            "100 KiB messages fill a 256 KiB batch at three"
        );

        prio_tx.send(Bytes::from_static(&[0xee])).await.unwrap();
        batch.clear();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(
            batch,
            vec![Bytes::from_static(&[0xee])],
            "control overtakes the five bulk messages still queued"
        );
    }

    #[tokio::test]
    async fn an_oversized_bulk_message_is_admitted_alone() {
        let (bulk_tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (_prio_tx, prio_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let mut lanes = WriterLanes::new(prio_rx, bulk_rx);
        bulk_tx.send(bulk(BULK_CHUNK_SIZE)).await.unwrap();
        bulk_tx.send(bulk(BULK_CHUNK_SIZE)).await.unwrap();

        let mut batch = Vec::new();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn the_bulk_batch_cap_fits_in_a_chunk() {
        const { assert!(BULK_BATCH_BYTES <= BULK_CHUNK_SIZE) };
    }

    #[test]
    fn upload_bytes_ride_bulk_and_the_handshake_rides_control() {
        assert!(
            ClientMessage::UploadChunk {
                request_id: 1,
                data: Bytes::from_static(&[0; 8]),
                offset: 0,
                is_final: false,
            }
            .is_bulk()
        );
        assert!(
            !ClientMessage::UploadRequest {
                job_id: "build:1".into(),
                request_id: 1,
                object: crate::types::UploadObject::Nar {
                    store_path: "/nix/store/aaa-foo".into()
                },
                size: 1,
            }
            .is_bulk()
        );
        assert!(!ClientMessage::UploadCancel { request_id: 1 }.is_bulk());
        assert!(
            !ServerMessage::UploadGrant {
                request_id: 1,
                target: crate::types::GrantTarget::Skip,
            }
            .is_bulk()
        );
    }

    fn nar_chunk(offset: u64) -> ServerMessage {
        ServerMessage::NarPush {
            job_id: "build:1".into(),
            store_path: "/nix/store/aaa-foo".into(),
            data: Bytes::from_static(&[0; 8]),
            offset,
            is_final: false,
        }
    }

    fn cache_reply() -> ServerMessage {
        ServerMessage::CacheStatus {
            query_id: "q1".into(),
            cached: Vec::new(),
        }
    }

    #[test]
    fn bulk_transfers_and_control_plane_take_different_lanes() {
        assert!(nar_chunk(0).is_bulk());
        assert!(
            ServerMessage::NarStreamHeader {
                job_id: "build:1".into(),
                store_path: "/nix/store/aaa-foo".into(),
                total_bytes: 1,
                stream_token: "t".into(),
            }
            .is_bulk()
        );
        assert!(!cache_reply().is_bulk());
        assert!(
            !ServerMessage::CacheError {
                query_id: "q1".into(),
                message: "boom".into(),
            }
            .is_bulk()
        );
    }

    #[test]
    fn a_job_completion_rides_behind_its_log_chunks() {
        assert!(
            ClientMessage::JobCompleted {
                job_id: "build:1".into(),
                assignment_id: "dispatch-1".into(),
                spans: Vec::new(),
                elapsed_ms: 0,
            }
            .is_bulk()
        );
    }

    #[tokio::test]
    async fn a_cache_reply_overtakes_nar_chunks_already_queued() {
        let (bulk_tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (prio_tx, prio_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let mut lanes = WriterLanes::new(prio_rx, bulk_rx);

        for i in 0..8u8 {
            bulk_tx.send(Bytes::from(vec![i])).await.unwrap();
        }
        prio_tx.send(Bytes::from_static(&[0xff])).await.unwrap();

        let mut batch = Vec::new();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(
            batch,
            vec![Bytes::from_static(&[0xff])],
            "the control lane must drain before queued bulk chunks, and must not \
             share a batch with them: the writer flushes only after feeding the \
             whole batch, so a chunk blocking mid-feed would strand the reply"
        );

        batch.clear();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(
            batch,
            (0..8u8).map(|i| Bytes::from(vec![i])).collect::<Vec<_>>(),
            "the bulk chunks follow in order, none dropped"
        );
    }

    #[tokio::test]
    async fn bulk_still_drains_and_the_writer_stops_when_both_lanes_close() {
        let (bulk_tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (prio_tx, prio_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let mut lanes = WriterLanes::new(prio_rx, bulk_rx);

        bulk_tx.send(Bytes::from_static(&[1])).await.unwrap();
        bulk_tx.send(Bytes::from_static(&[2])).await.unwrap();
        drop(bulk_tx);
        drop(prio_tx);

        let mut batch = Vec::new();
        assert!(lanes.next_batch(&mut batch).await);
        assert_eq!(
            batch,
            vec![Bytes::from_static(&[1]), Bytes::from_static(&[2])]
        );

        batch.clear();
        assert!(
            !lanes.next_batch(&mut batch).await,
            "both lanes closed and drained ends the writer task"
        );
    }

    #[tokio::test]
    async fn a_writer_that_never_sent_anything_stops_cleanly() {
        let (bulk_tx, bulk_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let (prio_tx, prio_rx) = mpsc::channel::<Bytes>(WRITER_QUEUE_DEPTH);
        let mut lanes = WriterLanes::new(prio_rx, bulk_rx);
        drop(bulk_tx);
        drop(prio_tx);

        let mut batch = Vec::new();
        assert!(!lanes.next_batch(&mut batch).await);
        assert!(batch.is_empty());
    }

    #[test]
    fn a_full_cache_query_and_its_worst_case_reply_stay_inflight_safe() {
        use crate::messages::{CACHE_QUERY_MAX_PATHS, CachedPath};

        let paths: Vec<String> = (0..CACHE_QUERY_MAX_PATHS)
            .map(|i| format!("/nix/store/{:0>32}-some-package-name-1.2.3-{i}", i))
            .collect();

        let query = ClientMessage::CacheQuery {
            job_id: "eval:019fcd73-aa63-7a41-b51c-05ed673c6d1f".to_owned(),
            query_id: "6c1a5e2c-0f52-4f9e-9a0e-2f1b7c4d8e90".to_owned(),
            paths: paths.clone(),
            mode: crate::types::QueryMode::Push,
            nar_sizes: vec![Some(u64::MAX); paths.len()],
            external: false,
        };

        let cached: Vec<CachedPath> = paths
            .iter()
            .map(|p| CachedPath {
                path: p.clone(),
                cached: false,
                file_size: Some(1),
                nar_size: Some(1),
                url: Some(format!("https://s3.example.com{p}?{}", "x".repeat(512))),
                nar_hash: Some(format!("sha256:{}", "y".repeat(52))),
                file_hash: Some(format!("sha256:{}", "z".repeat(52))),
                references: None,
                signatures: None,
                deriver: None,
                ca: None,
            })
            .collect();
        let reply = ServerMessage::CacheStatus {
            query_id: "6c1a5e2c-0f52-4f9e-9a0e-2f1b7c4d8e90".to_owned(),
            cached,
        };

        let newest = *PROTO_VERSIONS.end();
        let query_bytes = encode(&query, newest).expect("query encodes").len();
        let reply_bytes = encode(&reply, newest).expect("reply encodes").len();
        assert!(
            query_bytes <= SAFE_INFLIGHT_MESSAGE_SIZE,
            "CacheQuery of {CACHE_QUERY_MAX_PATHS} paths encodes to {query_bytes} bytes"
        );
        assert!(
            reply_bytes <= SAFE_INFLIGHT_MESSAGE_SIZE,
            "worst-case CacheStatus encodes to {reply_bytes} bytes"
        );
    }
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn unwired_writer(
        capacity: usize,
        timeout: Duration,
    ) -> (ProtoWriter, mpsc::Receiver<Bytes>, mpsc::Receiver<Bytes>) {
        let (tx, bulk_rx) = mpsc::channel::<Bytes>(capacity);
        let (control_tx, control_rx) = mpsc::channel::<Bytes>(capacity);
        (
            ProtoWriter {
                tx,
                control_tx,
                send_chunk_timeout: timeout,
                version: *PROTO_VERSIONS.end(),
                observer: None,
                _direction: PhantomData,
            },
            control_rx,
            bulk_rx,
        )
    }

    fn stalls(label: &str) -> i64 {
        STATS
            .snapshot()
            .iter()
            .filter(|(k, _)| k.metric == metric::PROTO_SEND_STALLS && k.label == label)
            .map(|(_, a)| a.count)
            .sum()
    }

    fn bulk_message() -> ServerMessage {
        ServerMessage::NarPush {
            job_id: "j".into(),
            store_path: "/nix/store/x".into(),
            data: Bytes::from_static(&[0]),
            offset: 0,
            is_final: true,
        }
    }

    #[tokio::test]
    async fn a_queued_bulk_message_raises_the_lane_peak() {
        let (writer, _rx) = MsgWriter::<ServerMessage>::spy(Duration::from_secs(1));
        for _ in 0..32 {
            writer.send_msg(&bulk_message()).await.expect("send");
        }

        assert!(GAUGES.bulk_lane_peak.get() >= 500);
    }

    #[tokio::test(start_paused = true)]
    async fn send_msg_times_out_when_queue_is_full() {
        let (writer, _control_rx, _bulk_rx) = unwired_writer(1, Duration::from_secs(5));
        writer
            .control_tx
            .send(Bytes::from_static(&[1, 2, 3]))
            .await
            .unwrap();

        let msg = ServerMessage::Reject {
            code: 400,
            reason: "stalled".into(),
        };
        let before = stalls("control");
        assert_eq!(
            writer.send_msg(&msg).await,
            Err(SendError::Stalled),
            "send_msg must report a stall when the writer queue stays full past send_chunk_timeout",
        );
        assert!(stalls("control") > before);
    }

    #[tokio::test]
    async fn send_msg_succeeds_when_queue_has_room() {
        let (writer, mut control_rx, _bulk_rx) = unwired_writer(2, Duration::from_secs(5));
        let msg = ServerMessage::Reject {
            code: 200,
            reason: "ok".into(),
        };
        writer.send_msg(&msg).await.expect("queue had room");
        let bytes = control_rx.try_recv().expect("byte buffer enqueued");
        assert!(!bytes.is_empty(), "serialised message should be non-empty");
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn a_traced_handshake_message_never_shows_its_tokens() {
        let authenticate = ServerMessage::Authenticate {
            worker_id: "w1".into(),
            tokens: vec![("p1".into(), "s3cret".into())],
        };
        let response = ClientMessage::AuthResponse {
            tokens: vec![("p1".into(), "s3cret".into())],
        };

        assert!(!format!("{:?}", Redacted(&authenticate)).contains("s3cret"));
        assert!(!format!("{:?}", Redacted(&response)).contains("s3cret"));
        assert!(format!("{:?}", Redacted(&ServerMessage::Draining)).contains("Draining"));
    }
}

#[cfg(test)]
mod first_message_tests {
    use super::*;
    use crate::messages::GradientCapabilities;

    fn first_message_of<M: WireMessage>(msg: &M) -> Option<FirstMessage> {
        let newest = *PROTO_VERSIONS.end();
        first_message(encode(msg, newest).expect("encodes"), newest)
    }

    #[test]
    fn a_worker_greeting_and_a_server_authenticate_are_told_apart() {
        let greeting = ClientMessage::InitConnection {
            capabilities: GradientCapabilities::default(),
            id: "w1".into(),
        };
        let authenticate = ServerMessage::Authenticate {
            worker_id: "w1".into(),
            tokens: vec![("p1".into(), "s3cret".into())],
        };

        assert!(matches!(
            first_message_of(&greeting),
            Some(FirstMessage::Worker(ClientMessage::InitConnection { .. }))
        ));
        assert!(matches!(
            first_message_of(&authenticate),
            Some(FirstMessage::Server(ServerMessage::Authenticate { .. }))
        ));
    }

    #[test]
    fn any_other_opening_frame_is_neither() {
        let response = ClientMessage::AuthResponse { tokens: vec![] };

        assert!(first_message_of(&response).is_none());
        assert!(first_message_of(&ServerMessage::Draining).is_none());
        assert!(first_message(Bytes::new(), *PROTO_VERSIONS.end()).is_none());
    }
}

#[cfg(test)]
mod agreement_tests {
    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::testing::loopback;

    #[tokio::test]
    async fn both_sides_agree_on_the_newest_shared_version_before_the_first_message() {
        let (mut accepted, mut dialing) = loopback().await;
        let (received, sent) = tokio::join!(
            accepted.recv_msg(),
            dialing.send_client_msg(&ClientMessage::RequestJobList),
        );

        assert_eq!(sent, Ok(()));
        assert_eq!(received, Some(ClientMessage::RequestJobList));
        assert_eq!(accepted.version(), Some(*PROTO_VERSIONS.end()));
        assert_eq!(dialing.version(), Some(*PROTO_VERSIONS.end()));
    }

    #[tokio::test]
    async fn a_peer_without_a_version_frame_is_closed_with_the_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let accept = async {
            let (tcp, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(MaybeTlsStream::Plain(tcp))
                .await
                .expect("upgrade");

            ProtoSocket::tungstenite(ws)
        };
        let (mut server, client) = tokio::join!(
            accept,
            tokio_tungstenite::connect_async(format!("ws://{addr}"))
        );

        let (mut client, _) = client.expect("dial");
        client
            .send(TungsteniteMessage::Binary(Bytes::from_static(b"rkyv")))
            .await
            .expect("send");

        assert_eq!(server.recv_msg().await, None);
        let close = loop {
            match client.next().await {
                Some(Ok(TungsteniteMessage::Close(frame))) => break frame,
                Some(Ok(_)) => continue,
                other => panic!("expected a close frame, got {other:?}"),
            }
        };
        let close = close.expect("a close frame with a reason");
        assert_eq!(close.code, CloseCode::Protocol);
        assert_eq!(close.reason.as_str(), "peer protocol is older than 27");
    }
}
