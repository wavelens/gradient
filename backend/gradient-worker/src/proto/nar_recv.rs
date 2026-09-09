/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Routing layer for incoming NAR transfers (server → worker) and the
//! push-resume gate (worker → server).
//!
//! When a job task sends `NarRequest`/`NarRequestResume` it then calls
//! [`NarReceiver::await_pending`] to await the assembled compressed NAR for
//! each path. The dispatch loop records the leading
//! [`gradient_proto::messages::ServerMessage::NarStreamHeader`] via
//! [`NarReceiver::note_header`] and hands every arriving
//! `ServerMessage::NarPush` frame to [`NarReceiver::accept_chunk`].
//!
//! A transfer is drained by its own staging task: the dispatch loop only moves
//! the frame onto a bounded channel, so no disk write ever runs on the loop.
//! When a [`gradient_storage::PartialStore`] is configured the task holds one
//! open [`gradient_storage::PartialWriter`] for the whole stream (keyed by job
//! and NAR hash) so an interrupted download can resume (issue #225) and the
//! compressed NAR is delivered as a file; otherwise it accumulates in memory
//! (used by tests). On `is_final` the [`NarPayload`] is delivered to the
//! waiting task via a `oneshot`.
//!
//! `NarUnavailable` / `NarAbort` are routed through [`NarReceiver::fail`] so
//! the waiter resolves with the reason immediately. The on-disk partial is
//! kept on failure so the next request can resume from where it stopped.
//!
//! For uploads, [`NarReceiver::register_push`] installs a one-shot gate that
//! the dispatch loop resolves on
//! [`gradient_proto::messages::ServerMessage::NarPushResume`], handing the
//! pusher the byte offset to seek to.

use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gradient_proto::messages::{ArchivedServerMessage, ServerMessage, TRANSFER_TIMEOUT};
use gradient_proto::session::frame::Frame;
use gradient_storage::{PartialStore, PartialWriter};
use tokio::sync::{mpsc, oneshot};
use tokio_util::task::TaskTracker;
use tracing::{debug, warn};

/// Ceiling on the push-resume handshake. A server that never answers a
/// `NarStreamHeader` falls back to a fresh upload from offset 0.
const PUSH_RESUME_TIMEOUT: Duration = Duration::from_secs(30);

/// Frames a staging task may queue before the dispatch loop has to wait. Deep
/// enough to absorb a burst, shallow enough that a stalled disk becomes
/// backpressure on the socket rather than unbounded memory.
const STAGE_QUEUE_DEPTH: usize = 8;

type Key = (String, String); // (job_id, store_path)

/// A received compressed NAR: staged on disk, or in memory when no partial
/// store is configured.
pub enum NarPayload {
    File(PathBuf),
    Bytes(Vec<u8>),
}

/// Never prints the payload itself: a NAR body in a log line is both useless
/// and unbounded.
impl std::fmt::Debug for NarPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NarPayload::File(path) => write!(f, "NarPayload::File({})", path.display()),
            NarPayload::Bytes(bytes) => write!(f, "NarPayload::Bytes({} bytes)", bytes.len()),
        }
    }
}

impl NarPayload {
    /// The compressed bytes, reading the staged file back when the transfer
    /// went to disk. Only for the small `.drv` payloads the closure walk mines;
    /// the import path decompresses straight from the file instead.
    pub(crate) async fn read_bytes(&self) -> Result<std::borrow::Cow<'_, [u8]>> {
        match self {
            NarPayload::Bytes(bytes) => Ok(std::borrow::Cow::Borrowed(bytes)),
            NarPayload::File(path) => Ok(std::borrow::Cow::Owned(
                tokio::fs::read(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("read staged NAR {}: {e}", path.display()))?,
            )),
        }
    }
}

/// Where a staging task puts the bytes of one transfer.
enum Sink {
    /// Open writer plus the partial key it was opened under, so the finished
    /// file can be claimed away from anything that might resume it. Boxed: the
    /// writer dwarfs the in-memory variant and every stream would carry it.
    Disk {
        writer: Box<PartialWriter>,
        key: String,
    },
    Memory(Vec<u8>),
}

impl Sink {
    async fn append(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        match self {
            Sink::Disk { writer, .. } => writer.append(offset, data).await,
            Sink::Memory(buf) => {
                buf.extend_from_slice(data);
                Ok(())
            }
        }
    }

    fn len(&self) -> u64 {
        match self {
            Sink::Disk { writer, .. } => writer.len(),
            Sink::Memory(buf) => buf.len() as u64,
        }
    }

    /// Close the sink and hand back what the importer should read. A disk sink
    /// claims its finished partial: the rename drops the resume token with it,
    /// so a repeat header for the same path cannot truncate the file the
    /// importer is about to read.
    async fn finish(self, partial: Option<&PartialStore>) -> Result<NarPayload> {
        match self {
            Sink::Memory(buf) => Ok(NarPayload::Bytes(buf)),
            Sink::Disk { writer, key } => {
                let staged = writer.finish().await?;
                let Some(store) = partial else {
                    return Ok(NarPayload::File(staged.path));
                };
                match store.detach(&key).await? {
                    Some(claim) => Ok(NarPayload::File(store.path(&claim))),
                    None => Ok(NarPayload::File(staged.path)),
                }
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Live transfers: the channel feeding each one's staging task.
    streams: HashMap<Key, mpsc::Sender<Frame<ServerMessage>>>,
    /// Outstanding pull waiters; resolved on `is_final` or on failure.
    waiters: HashMap<Key, oneshot::Sender<Result<NarPayload, String>>>,
    /// Outstanding push-resume gates; resolved on `NarPushResume`.
    push_waiters: HashMap<Key, oneshot::Sender<u64>>,
}

/// Shared state between the dispatch loop and job tasks for routing inbound
/// NARs and resolving push-resume handshakes.
#[derive(Clone, Default)]
pub struct NarReceiver {
    inner: Arc<Mutex<Inner>>,
    /// When set, pull chunks are staged to disk (keyed by job and NAR hash) so
    /// an interrupted download survives a reconnect. `None` keeps everything in
    /// memory (tests).
    partial: Option<PartialStore>,
    /// Registry for the per-transfer staging tasks, so they are owned by the
    /// receiver rather than detached into the runtime.
    stagers: TaskTracker,
}

/// Outstanding pull waiter handle returned by [`NarReceiver::register`].
pub struct PendingNar {
    job_id: String,
    store_path: String,
    rx: oneshot::Receiver<Result<NarPayload, String>>,
}

impl PendingNar {
    pub fn store_path(&self) -> &str {
        &self.store_path
    }
}

/// Gate awaiting a `NarPushResume`, returned by [`NarReceiver::register_push`].
pub struct PushResumeGate {
    rx: oneshot::Receiver<u64>,
}

impl PushResumeGate {
    /// Await the server's resume offset, defaulting to 0 (fresh upload) on
    /// timeout or a dropped connection.
    pub async fn await_resume(self) -> u64 {
        match tokio::time::timeout(PUSH_RESUME_TIMEOUT, self.rx).await {
            Ok(Ok(offset)) => offset,
            _ => 0,
        }
    }
}

/// Extract the 32-char store-hash from a `/nix/store/<hash>-name` path.
fn store_hash(store_path: &str) -> Option<&str> {
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()?;
    (hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(hash)
}

/// Partial-store key for a pull, namespaced by `job_id` so two concurrent jobs
/// on one worker transferring the *same* store path never share a `.partial`
/// file. Mirrors the server-push `{peer_id}/{hash}` namespacing; without it the
/// interleaved appends to a shared hash-keyed partial fail "non-contiguous" and
/// corrupt the staged NAR (only on the WS pull path - S3 pulls bypass staging).
fn partial_key(job_id: &str, store_path: &str) -> Option<String> {
    store_hash(store_path).map(|hash| format!("{job_id}/{hash}"))
}

/// Whether a sink continues from the prefix already on disk or truncates it.
#[derive(Clone, Copy)]
enum Resume {
    Staged,
    Fresh,
}

/// One transfer's identity: what a staging task needs to open, and if the
/// server restarts the stream, reopen, its sink.
struct StreamSpec {
    key: Key,
    /// `Some` in disk mode: the partial-store key this transfer stages under.
    disk_key: Option<String>,
    token: String,
    expected: Option<u64>,
}

/// Fail one transfer for a reason that makes its staged prefix unusable: drop
/// the partial so the next attempt starts clean rather than resuming garbage.
/// A server-signalled failure goes through [`NarReceiver::fail`] instead, which
/// keeps the prefix.
async fn abandon_staging(receiver: &NarReceiver, spec: &StreamSpec, reason: String) {
    if let (Some(store), Some(disk_key)) = (receiver.partial.as_ref(), spec.disk_key.as_deref())
        && let Err(e) = store.discard(disk_key).await
    {
        warn!(job_id = %spec.key.0, store_path = %spec.key.1, error = %e, "could not discard a failed NAR partial");
    }
    receiver.deliver(&spec.key, Err(reason));
}

/// Drain one transfer: every frame the dispatch loop queued is appended to
/// `sink`, and the final one resolves the waiter with the assembled payload.
async fn stage_pull(
    receiver: NarReceiver,
    spec: StreamSpec,
    mut sink: Sink,
    mut rx: mpsc::Receiver<Frame<ServerMessage>>,
) {
    let key = &spec.key;
    let mut started: Option<Instant> = None;

    while let Some(frame) = rx.recv().await {
        let ArchivedServerMessage::NarPush {
            data,
            offset,
            is_final,
            ..
        } = frame.archived()
        else {
            warn!(job_id = %key.0, store_path = %key.1, "non-NarPush frame on a NAR stream");
            continue;
        };

        let (data, offset, is_final) = (data.as_slice(), offset.to_native(), *is_final);

        // The server restarts from 0 when it decides our resume point is
        // unusable (a prefix longer than the object it holds), keeping the same
        // token, so drop the resumed prefix rather than fail on contiguity.
        // Only ever true before the first append.
        if offset == 0 && sink.len() != 0 {
            warn!(job_id = %key.0, store_path = %key.1, "server restarted the NAR transfer from 0");
            sink = match receiver.open_sink(&spec, Resume::Fresh).await {
                Ok(fresh) => fresh,
                Err(e) => {
                    abandon_staging(&receiver, &spec, format!("could not restage NAR: {e}")).await;
                    return;
                }
            };
        }

        if !data.is_empty() {
            started.get_or_insert_with(Instant::now);
            if let Err(e) = sink.append(offset, data).await {
                abandon_staging(&receiver, &spec, format!("partial append failed: {e}")).await;
                return;
            }
        }

        if !is_final {
            continue;
        }

        let staged = sink.len();
        if let Some(start) = started {
            crate::metrics::throughput::NETWORK.observe(
                staged as f64 * 8.0 / start.elapsed().as_secs_f64().max(1e-6) / 1_000_000.0,
            );
        }

        if let Some(total) = spec.expected
            && staged != total
        {
            drop(sink);
            abandon_staging(
                &receiver,
                &spec,
                format!("assembled NAR {staged} bytes != advertised {total} bytes"),
            )
            .await;
            return;
        }

        match sink.finish(receiver.partial.as_ref()).await {
            Ok(payload) => receiver.deliver(key, Ok(payload)),
            Err(e) => {
                abandon_staging(&receiver, &spec, format!("staging {} failed: {e}", key.1)).await;
            }
        }
        return;
    }

    // Retired without a final chunk (`fail`, `forget_job`, a dropped
    // connection): close the writer so the staged prefix is on disk and the
    // next attempt resumes from a length the file really holds.
    if let Sink::Disk { writer, .. } = sink
        && let Err(e) = writer.finish().await
    {
        warn!(job_id = %key.0, store_path = %key.1, error = %e, "flushing an interrupted NAR staging failed");
    }
}

impl NarReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Receiver that stages pull chunks to `store` for resumable downloads.
    pub fn with_partial_store(store: PartialStore) -> Self {
        Self {
            partial: Some(store),
            ..Self::default()
        }
    }

    /// Bytes already staged on disk for `store_path` and the token they were
    /// received under, if any. Returns `(0, None)` in memory-only mode or when
    /// nothing is staged - used by the requester to decide between
    /// `NarRequest` and `NarRequestResume`.
    pub async fn resumable(&self, job_id: &str, store_path: &str) -> (u64, Option<String>) {
        let Some(store) = self.partial.as_ref() else {
            return (0, None);
        };
        let Some(key) = partial_key(job_id, store_path) else {
            return (0, None);
        };
        (store.staged_len(&key).await, store.token(&key).await)
    }

    /// Synchronously install a waiter for `(job_id, store_path)`.
    pub fn register(&self, job_id: &str, store_path: &str) -> PendingNar {
        let key = (job_id.to_owned(), store_path.to_owned());
        let (tx, rx) = oneshot::channel();
        self.inner.lock().waiters.insert(key, tx);
        PendingNar {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            rx,
        }
    }

    /// Await a previously [`Self::register`]ed waiter, bounded by
    /// [`gradient_proto::messages::TRANSFER_TIMEOUT`].
    pub async fn await_pending(&self, pending: PendingNar) -> Result<NarPayload> {
        let PendingNar {
            job_id,
            store_path,
            rx,
        } = pending;
        let key = (job_id.clone(), store_path.clone());
        match tokio::time::timeout(TRANSFER_TIMEOUT, rx).await {
            Ok(Ok(Ok(payload))) => Ok(payload),
            Ok(Ok(Err(reason))) => Err(anyhow::anyhow!(
                "NAR transfer for {} failed: {}",
                store_path,
                reason
            )),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "NarPush waiter dropped for {}/{} - connection closed or superseded?",
                job_id,
                store_path
            )),
            Err(_) => {
                let mut g = self.inner.lock();
                g.streams.remove(&key);
                g.waiters.remove(&key);
                Err(anyhow::anyhow!(
                    "NarRequest for {} timed out after {}s waiting for NarPush \
                     (job_id={})",
                    store_path,
                    TRANSFER_TIMEOUT.as_secs(),
                    job_id,
                ))
            }
        }
    }

    /// Convenience: register + await in one step.
    #[cfg(test)]
    pub async fn wait_for(&self, job_id: &str, store_path: &str) -> Result<NarPayload> {
        let pending = self.register(job_id, store_path);
        self.await_pending(pending).await
    }

    /// Record the `NarStreamHeader` that precedes a pull's chunks and start the
    /// transfer's staging task. A repeat header for the same path retires the
    /// stream it supersedes.
    pub fn note_header(&self, job_id: &str, store_path: &str, total_bytes: u64, token: &str) {
        self.open_stream(self.spec(job_id, store_path, token, Some(total_bytes)));
    }

    /// The staging spec for one transfer. `disk_key` is `None` in memory mode
    /// and for a path whose store hash cannot be parsed.
    fn spec(
        &self,
        job_id: &str,
        store_path: &str,
        token: &str,
        expected: Option<u64>,
    ) -> StreamSpec {
        StreamSpec {
            key: (job_id.to_owned(), store_path.to_owned()),
            disk_key: self
                .partial
                .as_ref()
                .and_then(|_| partial_key(job_id, store_path)),
            token: token.to_owned(),
            expected,
        }
    }

    /// Queue one `NarPush` frame onto its transfer's staging task, opening a
    /// stream first when no header announced it (memory mode, tests). The queue
    /// is fed outside the lock, so a slow disk becomes backpressure on the
    /// socket instead of a stalled dispatch loop holding a mutex.
    pub async fn accept_chunk(&self, job_id: &str, store_path: &str, frame: Frame<ServerMessage>) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let existing = self.inner.lock().streams.get(&key).cloned();
        let tx = match existing {
            Some(tx) => tx,
            None => self.open_stream(self.spec(job_id, store_path, "", None)),
        };

        if tx.send(frame).await.is_err() {
            debug!(%job_id, %store_path, "NAR chunk for a finished stream - discarding");
        }
    }

    /// Install a stream for `spec.key` and spawn its staging task, returning
    /// the sender. Any stream already registered under that key is dropped,
    /// which ends its task.
    fn open_stream(&self, spec: StreamSpec) -> mpsc::Sender<Frame<ServerMessage>> {
        let (tx, rx) = mpsc::channel(STAGE_QUEUE_DEPTH);
        self.inner
            .lock()
            .streams
            .insert(spec.key.clone(), tx.clone());

        let receiver = self.clone();
        self.stagers.spawn(async move {
            match receiver.open_sink(&spec, Resume::Staged).await {
                Ok(sink) => stage_pull(receiver, spec, sink, rx).await,
                Err(e) => {
                    receiver.deliver(&spec.key, Err(format!("could not stage NAR: {e}")));
                }
            }
        });
        tx
    }

    /// Open the sink a staging task writes into. `Resume::Staged` continues
    /// from whatever prefix the store already holds under the stream's token,
    /// which is the offset the requester told the server to continue from.
    async fn open_sink(&self, spec: &StreamSpec, resume: Resume) -> Result<Sink> {
        let (Some(store), Some(disk_key)) = (self.partial.as_ref(), spec.disk_key.as_deref())
        else {
            return Ok(Sink::Memory(Vec::new()));
        };

        let resume_from = match resume {
            Resume::Staged => store.received_len(disk_key, &spec.token).await.unwrap_or(0),
            Resume::Fresh => 0,
        };
        let writer = store
            .open_writer(disk_key, &spec.token, resume_from)
            .await?;
        debug!(job_id = %spec.key.0, store_path = %spec.key.1, resume_from, "staging pulled NAR to disk");
        Ok(Sink::Disk {
            writer: Box::new(writer),
            key: disk_key.to_owned(),
        })
    }

    /// Resolve the waiter for `key`, warning if none is registered.
    fn deliver(&self, key: &Key, result: Result<NarPayload, String>) {
        let mut g = self.inner.lock();
        g.streams.remove(key);
        match g.waiters.remove(key) {
            Some(tx) => {
                if tx.send(result).is_err() {
                    debug!(job_id = %key.0, store_path = %key.1, "NAR waiter went away before delivery");
                }
            }
            None => {
                warn!(job_id = %key.0, store_path = %key.1, "NAR delivery with no waiter - discarding");
            }
        }
    }

    /// Resolve the waiter for `(job_id, store_path)` with an error. Called for
    /// both `NarUnavailable` and `NarAbort`. Any on-disk partial is kept so a
    /// later request can resume from where it stopped.
    pub fn fail(&self, job_id: &str, store_path: &str, reason: String) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let mut g = self.inner.lock();
        g.streams.remove(&key);
        match g.waiters.remove(&key) {
            Some(tx) => {
                if tx.send(Err(reason)).is_err() {
                    debug!(%job_id, %store_path, "NAR failure waiter went away before delivery");
                }
            }
            None => {
                warn!(%job_id, %store_path, %reason, "NarUnavailable/NarAbort with no waiter - discarding");
            }
        }
    }

    /// Install a push-resume gate before sending a `NarStreamHeader`.
    pub fn register_push(&self, job_id: &str, store_path: &str) -> PushResumeGate {
        let key = (job_id.to_owned(), store_path.to_owned());
        let (tx, rx) = oneshot::channel();
        self.inner.lock().push_waiters.insert(key, tx);
        PushResumeGate { rx }
    }

    /// Resolve a push-resume gate with the server's `received_bytes`.
    pub fn resolve_push(&self, job_id: &str, store_path: &str, received_bytes: u64) {
        let key = (job_id.to_owned(), store_path.to_owned());
        if let Some(tx) = self.inner.lock().push_waiters.remove(&key) {
            let _ = tx.send(received_bytes);
        } else {
            debug!(%job_id, %store_path, "NarPushResume with no push gate - discarding");
        }
    }

    /// Drop in-memory state for a job, ending its staging tasks. On-disk
    /// partials (keyed by job and hash) are left for the GC sweep so a later
    /// attempt can still resume.
    pub fn forget_job(&self, job_id: &str) {
        let mut g = self.inner.lock();
        g.streams.retain(|(j, _), _| j != job_id);
        g.waiters.retain(|(j, _), _| j != job_id);
        g.push_waiters.retain(|(j, _), _| j != job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_proto::session::frame::{Inbound, WireMessage};
    use tempfile::TempDir;

    fn frame(
        job: &str,
        path: &str,
        offset: u64,
        data: &[u8],
        is_final: bool,
    ) -> Frame<ServerMessage> {
        let msg = ServerMessage::NarPush {
            job_id: job.into(),
            store_path: path.into(),
            data: data.to_vec(),
            offset,
            is_final,
        };
        match ServerMessage::decode(msg.encode().expect("encodes")).expect("decodes") {
            Inbound::Bulk(f) => f,
            Inbound::Control(_) => panic!("NarPush is bulk"),
        }
    }

    async fn final_chunk(r: &NarReceiver, job: &str, path: &str, data: &[u8]) {
        r.accept_chunk(job, path, frame(job, path, 0, data, true))
            .await;
    }

    fn bytes(payload: NarPayload) -> Vec<u8> {
        match payload {
            NarPayload::Bytes(b) => b,
            NarPayload::File(p) => panic!("memory mode yields bytes, got {}", p.display()),
        }
    }

    /// Let every staging task started so far run to completion. A stager is a
    /// separate task now, so a delivery is no longer ordered against the
    /// caller of `accept_chunk`.
    async fn settle(r: &NarReceiver) {
        r.stagers.close();
        r.stagers.wait().await;
    }

    async fn file_bytes(payload: NarPayload) -> Vec<u8> {
        match payload {
            NarPayload::File(p) => tokio::fs::read(&p).await.unwrap(),
            NarPayload::Bytes(_) => panic!("disk mode yields a file"),
        }
    }

    #[tokio::test]
    async fn single_chunk_delivers_to_waiter() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("job1", "/nix/store/aaa").await });
        tokio::task::yield_now().await;
        final_chunk(&r, "job1", "/nix/store/aaa", b"hello world").await;
        assert_eq!(bytes(task.await.unwrap().unwrap()), b"hello world");
    }

    #[tokio::test]
    async fn multi_chunk_assembled_in_order() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.accept_chunk(
            "j",
            "/nix/store/x",
            frame("j", "/nix/store/x", 0, b"abc", false),
        )
        .await;
        r.accept_chunk(
            "j",
            "/nix/store/x",
            frame("j", "/nix/store/x", 3, b"def", false),
        )
        .await;
        r.accept_chunk(
            "j",
            "/nix/store/x",
            frame("j", "/nix/store/x", 6, b"ghi", true),
        )
        .await;
        assert_eq!(bytes(task.await.unwrap().unwrap()), b"abcdefghi");
    }

    /// With a partial store the transfer never sits in memory: the waiter is
    /// handed the staged file, holding exactly the bytes that were pushed.
    #[tokio::test]
    async fn disk_mode_delivers_the_staged_file() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path(), Duration::from_secs(60)).unwrap();
        let r = NarReceiver::with_partial_store(store);
        let path = format!("/nix/store/{}-x", "a".repeat(32));
        let r2 = r.clone();
        let p = path.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", &p).await });
        tokio::task::yield_now().await;
        r.note_header("j", &path, 6, "len-6");
        r.accept_chunk("j", &path, frame("j", &path, 0, b"abc", false))
            .await;
        r.accept_chunk("j", &path, frame("j", &path, 3, b"def", true))
            .await;
        assert_eq!(file_bytes(task.await.unwrap().unwrap()).await, b"abcdef");
    }

    #[tokio::test]
    async fn final_with_no_waiter_is_discarded() {
        let r = NarReceiver::new();
        final_chunk(&r, "j", "/nix/store/x", b"orphan").await;
        settle(&r).await;
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        final_chunk(&r, "j", "/nix/store/x", b"second").await;
        assert_eq!(bytes(task.await.unwrap().unwrap()), b"second");
    }

    #[tokio::test]
    async fn forget_job_cancels_waiters() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("doomed", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.forget_job("doomed");
        assert!(
            task.await.unwrap().is_err(),
            "waiter should have been cancelled"
        );
    }

    #[tokio::test]
    async fn fail_resolves_waiter_with_reason() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.fail("j", "/nix/store/x", "not in nar_storage".into());
        let err = task.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("not in nar_storage"), "got: {err}");
    }

    #[tokio::test]
    async fn register_synchronously_installs_waiter_before_response() {
        let r = NarReceiver::new();
        let p1 = r.register("job", "/nix/store/a");
        let p2 = r.register("job", "/nix/store/b");

        r.fail("job", "/nix/store/a", "missing".into());
        final_chunk(&r, "job", "/nix/store/b", b"hello").await;

        let r1 = r.await_pending(p1).await;
        assert!(r1.unwrap_err().to_string().contains("missing"));
        assert_eq!(bytes(r.await_pending(p2).await.unwrap()), b"hello");
    }

    /// A `PartialStore`-backed receiver resumes across a simulated reconnect:
    /// the first attempt stages bytes to disk and fails; a fresh receiver over
    /// the same root reports the staged prefix and completes the transfer.
    #[tokio::test]
    async fn partial_store_resumes_across_reconnect() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let path = format!("/nix/store/{hash}-pkg");

        let r1 = NarReceiver::with_partial_store(store.clone());
        r1.note_header("j", &path, 9, "len-9");
        r1.accept_chunk("j", &path, frame("j", &path, 0, b"abc", false))
            .await;
        r1.accept_chunk("j", &path, frame("j", &path, 3, b"def", false))
            .await;
        // Connection drops mid-transfer.
        r1.fail("j", &path, "NarAbort".into());
        settle(&r1).await;

        let (staged, token) = r1.resumable("j", &path).await;
        assert_eq!(staged, 6);
        assert_eq!(token.as_deref(), Some("len-9"));

        // Fresh receiver (reconnect) resumes from offset 6 and completes.
        let r2 = NarReceiver::with_partial_store(store);
        let r2c = r2.clone();
        let pathc = path.clone();
        let task = tokio::spawn(async move { r2c.wait_for("j", &pathc).await });
        tokio::task::yield_now().await;
        r2.note_header("j", &path, 9, "len-9");
        r2.accept_chunk("j", &path, frame("j", &path, 6, b"ghi", true))
            .await;
        assert_eq!(file_bytes(task.await.unwrap().unwrap()).await, b"abcdefghi");
    }

    /// Two jobs transferring the SAME store path concurrently must not share a
    /// partial file. With a bare-hash key their interleaved appends corrupted
    /// the partial and failed "non-contiguous"; per-`{job_id}/{hash}` keys keep
    /// them isolated so both assemble correctly.
    #[tokio::test]
    async fn concurrent_jobs_same_path_do_not_collide() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
        let hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let path = format!("/nix/store/{hash}-pkg");

        let r = NarReceiver::with_partial_store(store);
        r.note_header("j1", &path, 6, "t1");
        r.note_header("j2", &path, 6, "t2");
        let p1 = r.register("j1", &path);
        let p2 = r.register("j2", &path);

        // Interleave both jobs' chunks for the same hash: under a shared key
        // j2's offset-0 chunk would truncate j1's bytes and the offset-3 chunks
        // would then fail the contiguity check.
        r.accept_chunk("j1", &path, frame("j1", &path, 0, b"aaa", false))
            .await;
        r.accept_chunk("j2", &path, frame("j2", &path, 0, b"bbb", false))
            .await;
        r.accept_chunk("j1", &path, frame("j1", &path, 3, b"AAA", true))
            .await;
        r.accept_chunk("j2", &path, frame("j2", &path, 3, b"BBB", true))
            .await;

        assert_eq!(
            file_bytes(r.await_pending(p1).await.unwrap()).await,
            b"aaaAAA"
        );
        assert_eq!(
            file_bytes(r.await_pending(p2).await.unwrap()).await,
            b"bbbBBB"
        );
    }

    /// The server restarts a transfer from 0 (our staged prefix is longer than
    /// the object it holds) while echoing the same token. The stale prefix must
    /// be dropped instead of failing the contiguity check.
    #[tokio::test]
    async fn a_restart_from_zero_drops_the_resumed_prefix() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
        let hash = "cccccccccccccccccccccccccccccccc";
        let path = format!("/nix/store/{hash}-pkg");
        store
            .append(&format!("j/{hash}"), "len-4", 0, b"stale!")
            .await
            .unwrap();

        let r = NarReceiver::with_partial_store(store);
        let pending = r.register("j", &path);
        r.note_header("j", &path, 4, "len-4");
        r.accept_chunk("j", &path, frame("j", &path, 0, b"abcd", false))
            .await;
        r.accept_chunk("j", &path, frame("j", &path, 4, b"", true))
            .await;

        assert_eq!(
            file_bytes(r.await_pending(pending).await.unwrap()).await,
            b"abcd"
        );
    }

    #[tokio::test]
    async fn push_resume_gate_resolves() {
        let r = NarReceiver::new();
        let gate = r.register_push("j", "/nix/store/x");
        r.resolve_push("j", "/nix/store/x", 4096);
        assert_eq!(gate.await_resume().await, 4096);
    }

    #[tokio::test]
    async fn push_resume_gate_defaults_to_zero_without_answer() {
        let r = NarReceiver::new();
        let gate = r.register_push("j", "/nix/store/x");
        drop(r); // server never answers
        assert_eq!(gate.await_resume().await, 0);
    }
}
