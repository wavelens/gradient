/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Instant;

use anyhow::Result;
use gradient_storage::{PartialStore, PartialWriter};
use gradient_wire::messages::{ArchivedServerMessage, ServerMessage, TRANSFER_TIMEOUT};
use gradient_wire::session::frame::{Frame, Inbound};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;
use tracing::{debug, warn};

const STAGE_QUEUE_DEPTH: usize = 8;

type Key = (String, String);

pub enum NarPayload {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl std::fmt::Debug for NarPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NarPayload::File(path) => write!(f, "NarPayload::File({})", path.display()),
            NarPayload::Bytes(bytes) => write!(f, "NarPayload::Bytes({} bytes)", bytes.len()),
        }
    }
}

impl NarPayload {
    pub async fn read_bytes(&self) -> Result<std::borrow::Cow<'_, [u8]>> {
        match self {
            NarPayload::Bytes(bytes) => Ok(std::borrow::Cow::Borrowed(bytes)),
            NarPayload::File(path) => Ok(std::borrow::Cow::Owned(
                tokio::fs::read(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("read staged NAR {}: {e}", path.display()))?,
            )),
        }
    }

    pub async fn byte_len(&self) -> u64 {
        match self {
            NarPayload::Bytes(bytes) => bytes.len() as u64,
            NarPayload::File(path) => tokio::fs::metadata(path).await.map_or(0, |m| m.len()),
        }
    }
}

enum Sink {
    Disk {
        writer: Box<PartialWriter>,
        store: PartialStore,
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

    async fn finish(self) -> Result<NarPayload> {
        match self {
            Sink::Memory(buf) => Ok(NarPayload::Bytes(buf)),
            Sink::Disk { writer, store, key } => {
                let staged = writer.finish().await?;
                match store.detach(&key).await? {
                    Some(claim) => Ok(NarPayload::File(store.path(&claim))),
                    None => Ok(NarPayload::File(staged.path)),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferFailure {
    Unavailable(String),
    Transient(String),
}

#[derive(Debug)]
pub struct NarUnavailable {
    pub store_path: String,
    pub reason: String,
}

impl std::fmt::Display for NarUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the cache cannot serve {}: {}",
            self.store_path, self.reason
        )
    }
}

impl std::error::Error for NarUnavailable {}

#[derive(Default)]
struct Inner {
    streams: HashMap<Key, mpsc::Sender<Frame<ServerMessage>>>,
    waiters: HashMap<Key, Waiter>,
}

#[derive(Clone, Default)]
pub struct NarReceiver {
    inner: Arc<Mutex<Inner>>,
    partial: Option<PartialStore>,
    stagers: TaskTracker,
}

struct Waiter {
    tx: oneshot::Sender<Result<NarPayload, TransferFailure>>,
    progress: watch::Sender<u64>,
}

pub struct PendingNar {
    job_id: String,
    store_path: String,
    rx: oneshot::Receiver<Result<NarPayload, TransferFailure>>,
    progress: watch::Receiver<u64>,
}

impl PendingNar {
    pub fn store_path(&self) -> &str {
        &self.store_path
    }
}

fn store_hash(store_path: &str) -> Option<&str> {
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()?;
    (hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(hash)
}

/// The key is namespaced by `job_id` because two jobs can transfer the same store path at once.
/// Interleaved appends to a shared partial would fail "non-contiguous" and corrupt the staged NAR.
fn partial_key(job_id: &str, store_path: &str) -> Option<String> {
    store_hash(store_path).map(|hash| format!("{job_id}/{hash}"))
}

#[derive(Clone, Copy)]
enum Resume {
    Staged,
    Fresh,
}

struct StreamSpec {
    key: Key,
    disk_key: Option<String>,
    token: String,
    expected: Option<u64>,
    progress: Option<watch::Sender<u64>>,
}

/// The link back is weak on purpose. A full clone would keep its own sender alive, and a lost
/// connection would leak the task and its open `.partial`.
struct Stager {
    inner: Weak<Mutex<Inner>>,
    partial: Option<PartialStore>,
}

impl Stager {
    async fn deliver(&self, key: &Key, result: Result<NarPayload, TransferFailure>) {
        let undelivered = match self.inner.upgrade() {
            None => Some(result),
            Some(inner) => {
                let mut g = inner.lock();
                g.streams.remove(key);
                match g.waiters.remove(key) {
                    Some(waiter) => waiter.tx.send(result).err().inspect(|_| {
                        debug!(job_id = %key.0, store_path = %key.1, "NAR waiter went away before delivery");
                    }),
                    None => {
                        warn!(job_id = %key.0, store_path = %key.1, "NAR delivery with no waiter - discarding");
                        Some(result)
                    }
                }
            }
        };

        if let Some(Ok(NarPayload::File(path))) = undelivered
            && let Err(e) = tokio::fs::remove_file(&path).await
        {
            warn!(path = %path.display(), error = %e, "could not remove an undeliverable staged NAR");
        }
    }

    async fn abandon(&self, spec: &StreamSpec, reason: String) {
        if let (Some(store), Some(disk_key)) = (self.partial.as_ref(), spec.disk_key.as_deref())
            && let Err(e) = store.discard(disk_key).await
        {
            warn!(job_id = %spec.key.0, store_path = %spec.key.1, error = %e, "could not discard a failed NAR partial");
        }
        self.deliver(&spec.key, Err(TransferFailure::Transient(reason)))
            .await;
    }

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
            .open_writer(disk_key, &spec.token, resume_from, 0)
            .await?;
        debug!(job_id = %spec.key.0, store_path = %spec.key.1, resume_from, "staging pulled NAR to disk");
        Ok(Sink::Disk {
            writer: Box::new(writer),
            store: store.clone(),
            key: disk_key.to_owned(),
        })
    }
}

async fn stage_pull(
    stager: Stager,
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

        if offset == 0 && sink.len() != 0 {
            warn!(job_id = %key.0, store_path = %key.1, "server restarted the NAR transfer from 0");
            sink = match stager.open_sink(&spec, Resume::Fresh).await {
                Ok(fresh) => fresh,
                Err(e) => {
                    stager
                        .abandon(&spec, format!("could not restage NAR: {e}"))
                        .await;
                    return;
                }
            };
        }

        if !data.is_empty() {
            started.get_or_insert_with(Instant::now);
            if let Err(e) = sink.append(offset, data).await {
                stager
                    .abandon(&spec, format!("partial append failed: {e}"))
                    .await;
                return;
            }
            if let Some(progress) = &spec.progress {
                progress.send_modify(|staged| *staged += data.len() as u64);
            }
        }

        if !is_final {
            continue;
        }

        let staged = sink.len();
        if let Some(start) = started {
            crate::throughput::NETWORK.observe_transfer(staged, start.elapsed());
        }

        if let Some(total) = spec.expected
            && staged != total
        {
            drop(sink);
            stager
                .abandon(
                    &spec,
                    format!("assembled NAR {staged} bytes != advertised {total} bytes"),
                )
                .await;
            return;
        }

        match sink.finish().await {
            Ok(payload) => stager.deliver(key, Ok(payload)).await,
            Err(e) => {
                stager
                    .abandon(&spec, format!("staging {} failed: {e}", key.1))
                    .await;
            }
        }
        return;
    }

    if let Sink::Disk { writer, .. } = sink
        && let Err(e) = writer.finish().await
    {
        warn!(job_id = %key.0, store_path = %key.1, error = %e, "flushing an interrupted NAR staging failed");
    }
}

async fn stalled(mut progress: watch::Receiver<u64>) {
    while let Ok(Ok(())) = tokio::time::timeout(TRANSFER_TIMEOUT, progress.changed()).await {}
    if progress.has_changed().is_err() {
        std::future::pending::<()>().await;
    }
}

impl NarReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_partial_store(store: PartialStore) -> Self {
        Self {
            partial: Some(store),
            ..Self::default()
        }
    }

    pub async fn resumable(&self, job_id: &str, store_path: &str) -> (u64, Option<String>) {
        let Some(store) = self.partial.as_ref() else {
            return (0, None);
        };
        let Some(key) = partial_key(job_id, store_path) else {
            return (0, None);
        };
        (store.staged_len(&key).await, store.token(&key).await)
    }

    pub fn register(&self, job_id: &str, store_path: &str) -> PendingNar {
        let key = (job_id.to_owned(), store_path.to_owned());
        let (tx, rx) = oneshot::channel();
        let (progress, progress_rx) = watch::channel(0);
        self.inner
            .lock()
            .waiters
            .insert(key, Waiter { tx, progress });
        PendingNar {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            rx,
            progress: progress_rx,
        }
    }

    pub async fn await_pending(&self, pending: PendingNar) -> Result<NarPayload> {
        let PendingNar {
            job_id,
            store_path,
            rx,
            progress,
        } = pending;
        let key = (job_id.clone(), store_path.clone());
        let outcome = tokio::select! {
            delivered = rx => Ok(delivered),
            () = stalled(progress) => Err(()),
        };
        match outcome {
            Ok(Ok(Ok(payload))) => Ok(payload),
            Ok(Ok(Err(TransferFailure::Unavailable(reason)))) => {
                Err(anyhow::Error::new(NarUnavailable { store_path, reason }))
            }
            Ok(Ok(Err(TransferFailure::Transient(reason)))) => Err(anyhow::anyhow!(
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
                    "NarRequest for {} made no progress for {}s waiting for NarPush \
                     (job_id={})",
                    store_path,
                    TRANSFER_TIMEOUT.as_secs(),
                    job_id,
                ))
            }
        }
    }

    #[cfg(test)]
    pub async fn wait_for(&self, job_id: &str, store_path: &str) -> Result<NarPayload> {
        let pending = self.register(job_id, store_path);
        self.await_pending(pending).await
    }

    pub fn note_header(&self, job_id: &str, store_path: &str, total_bytes: u64, token: &str) {
        self.open_stream(self.spec(job_id, store_path, token, Some(total_bytes)));
    }

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
            progress: None,
        }
    }

    pub async fn accept_chunk(&self, job_id: &str, store_path: &str, frame: Frame<ServerMessage>) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let tx = {
            let g = self.inner.lock();
            match g.streams.get(&key).cloned() {
                Some(tx) => Some(tx),
                None => {
                    let requested = g.waiters.contains_key(&key);
                    drop(g);
                    requested.then(|| self.open_stream(self.spec(job_id, store_path, "", None)))
                }
            }
        };

        let Some(tx) = tx else {
            warn!(%job_id, %store_path, "NarPush for a path nothing requested - discarding");
            return;
        };

        if tx.send(frame).await.is_err() {
            debug!(%job_id, %store_path, "NAR chunk for a finished stream - discarding");
        }
    }

    fn open_stream(&self, mut spec: StreamSpec) -> mpsc::Sender<Frame<ServerMessage>> {
        let (tx, rx) = mpsc::channel(STAGE_QUEUE_DEPTH);
        {
            let mut g = self.inner.lock();
            spec.progress = g.waiters.get(&spec.key).map(|w| w.progress.clone());
            g.streams.insert(spec.key.clone(), tx.clone());
        }

        let stager = Stager {
            inner: Arc::downgrade(&self.inner),
            partial: self.partial.clone(),
        };
        self.stagers.spawn(async move {
            match stager.open_sink(&spec, Resume::Staged).await {
                Ok(sink) => stage_pull(stager, spec, sink, rx).await,
                Err(e) => {
                    stager
                        .deliver(
                            &spec.key,
                            Err(TransferFailure::Transient(format!(
                                "could not stage NAR: {e}"
                            ))),
                        )
                        .await;
                }
            }
        });
        tx
    }

    pub fn fail(&self, job_id: &str, store_path: &str, failure: TransferFailure) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let reason = match &failure {
            TransferFailure::Unavailable(r) | TransferFailure::Transient(r) => r.clone(),
        };
        let mut g = self.inner.lock();
        g.streams.remove(&key);
        match g.waiters.remove(&key) {
            Some(waiter) => {
                if waiter.tx.send(Err(failure)).is_err() {
                    debug!(%job_id, %store_path, "NAR failure waiter went away before delivery");
                }
            }
            None => {
                warn!(%job_id, %store_path, %reason, "NarUnavailable/NarAbort with no waiter - discarding");
            }
        }
    }

    pub fn forget_job(&self, job_id: &str) {
        let mut g = self.inner.lock();
        g.streams.retain(|(j, _), _| j != job_id);
        g.waiters.retain(|(j, _), _| j != job_id);
    }

    pub async fn absorb(&self, inbound: Inbound<ServerMessage>) -> Option<Inbound<ServerMessage>> {
        match inbound {
            Inbound::Bulk(frame) => {
                let ArchivedServerMessage::NarPush {
                    job_id,
                    store_path,
                    offset,
                    is_final,
                    data,
                } = frame.archived()
                else {
                    return Some(Inbound::Bulk(frame));
                };

                debug!(
                    job_id = job_id.as_str(),
                    store_path = store_path.as_str(),
                    offset = offset.to_native(),
                    is_final = *is_final,
                    bytes = data.len(),
                    "received NAR chunk"
                );
                let (job_id, store_path) = (job_id.to_string(), store_path.to_string());
                self.accept_chunk(&job_id, &store_path, frame).await;
                None
            }
            Inbound::Control(ServerMessage::NarStreamHeader {
                job_id,
                store_path,
                total_bytes,
                stream_token,
            }) => {
                self.note_header(&job_id, &store_path, total_bytes, &stream_token);
                None
            }
            Inbound::Control(ServerMessage::NarUnavailable {
                job_id,
                store_path,
                reason,
            }) => {
                warn!(%job_id, %store_path, %reason, "the cache cannot serve this NAR");
                self.fail(&job_id, &store_path, TransferFailure::Unavailable(reason));
                None
            }
            Inbound::Control(ServerMessage::NarAbort {
                job_id,
                store_path,
                reason,
            }) => {
                warn!(%job_id, %store_path, %reason, "server aborted a NAR transfer");
                self.fail(&job_id, &store_path, TransferFailure::Transient(reason));
                None
            }
            other => Some(other),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_wire::session::frame::{Inbound, WireMessage};
    use std::time::Duration;
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
    async fn a_payload_measures_its_transferred_bytes() {
        let dir = TempDir::new().unwrap();
        let staged = dir.path().join("nar");
        tokio::fs::write(&staged, b"compressed nar").await.unwrap();

        assert_eq!(NarPayload::File(staged).byte_len().await, 14);
        assert_eq!(NarPayload::Bytes(vec![0; 3]).byte_len().await, 3);
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

    #[tokio::test(start_paused = true)]
    async fn a_transfer_that_keeps_progressing_outlives_the_transfer_timeout() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/big").await });
        tokio::task::yield_now().await;

        for i in 0..3u64 {
            r.accept_chunk(
                "j",
                "/nix/store/big",
                frame("j", "/nix/store/big", i, b"x", false),
            )
            .await;
            tokio::time::advance(TRANSFER_TIMEOUT / 2).await;
        }
        final_chunk_at(&r, "j", "/nix/store/big", 3, b"y").await;

        assert_eq!(bytes(task.await.unwrap().unwrap()), b"xxxy");
    }

    #[tokio::test(start_paused = true)]
    async fn a_transfer_without_progress_times_out() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/stuck").await });
        tokio::task::yield_now().await;
        r.accept_chunk(
            "j",
            "/nix/store/stuck",
            frame("j", "/nix/store/stuck", 0, b"x", false),
        )
        .await;

        tokio::time::advance(TRANSFER_TIMEOUT + Duration::from_secs(1)).await;

        let err = task.await.unwrap().expect_err("stalled transfer fails");
        assert!(err.to_string().contains("no progress"), "{err}");
    }

    async fn final_chunk_at(r: &NarReceiver, job: &str, path: &str, offset: u64, data: &[u8]) {
        r.accept_chunk(job, path, frame(job, path, offset, data, true))
            .await;
    }

    #[tokio::test]
    async fn disk_mode_delivers_the_staged_file() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
        let store_for_key = store.clone();
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

        let payload = task.await.unwrap().unwrap();
        let NarPayload::File(delivered) = &payload else {
            panic!("disk mode yields a file");
        };
        assert_ne!(
            delivered,
            &store_for_key.path(&format!("j/{}", "a".repeat(32)))
        );
        assert_eq!(r.resumable("j", &path).await.0, 0);
        assert_eq!(file_bytes(payload).await, b"abcdef");
    }

    #[tokio::test]
    async fn final_with_no_waiter_is_discarded() {
        let r = NarReceiver::new();
        final_chunk(&r, "j", "/nix/store/x", b"orphan").await;
        assert!(r.inner.lock().streams.is_empty(), "no waiter, no stream");
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
        r.fail(
            "j",
            "/nix/store/x",
            TransferFailure::Transient("not in nar_storage".into()),
        );
        let err = task.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("not in nar_storage"), "got: {err}");
    }

    #[tokio::test]
    async fn an_unavailable_nar_is_typed_apart_from_an_aborted_transfer() {
        let r = NarReceiver::new();
        let gone = r.register("j", "/nix/store/gone");
        let dropped = r.register("j", "/nix/store/dropped");

        r.fail(
            "j",
            "/nix/store/gone",
            TransferFailure::Unavailable("NAR not found in cache".into()),
        );
        r.fail(
            "j",
            "/nix/store/dropped",
            TransferFailure::Transient("NarAbort".into()),
        );

        let gone = r.await_pending(gone).await.unwrap_err();
        assert_eq!(
            gone.downcast_ref::<NarUnavailable>().map(|u| &u.store_path),
            Some(&"/nix/store/gone".to_owned()),
        );
        assert!(
            r.await_pending(dropped)
                .await
                .unwrap_err()
                .downcast_ref::<NarUnavailable>()
                .is_none(),
            "an abort stays a transport failure"
        );
    }

    #[tokio::test]
    async fn register_synchronously_installs_waiter_before_response() {
        let r = NarReceiver::new();
        let p1 = r.register("job", "/nix/store/a");
        let p2 = r.register("job", "/nix/store/b");

        r.fail(
            "job",
            "/nix/store/a",
            TransferFailure::Transient("missing".into()),
        );
        final_chunk(&r, "job", "/nix/store/b", b"hello").await;

        let r1 = r.await_pending(p1).await;
        assert!(r1.unwrap_err().to_string().contains("missing"));
        assert_eq!(bytes(r.await_pending(p2).await.unwrap()), b"hello");
    }

    #[tokio::test]
    async fn partial_store_resumes_across_reconnect() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let path = format!("/nix/store/{hash}-pkg");

        let r1 = NarReceiver::with_partial_store(store.clone());
        r1.note_header("j", &path, 9, "len-9");
        r1.accept_chunk("j", &path, frame("j", &path, 0, b"abc", false))
            .await;
        r1.accept_chunk("j", &path, frame("j", &path, 3, b"def", false))
            .await;
        r1.fail("j", &path, TransferFailure::Transient("NarAbort".into()));
        settle(&r1).await;

        let (staged, token) = r1.resumable("j", &path).await;
        assert_eq!(staged, 6);
        assert_eq!(token.as_deref(), Some("len-9"));

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

    #[tokio::test]
    async fn concurrent_jobs_same_path_do_not_collide() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
        let hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let path = format!("/nix/store/{hash}-pkg");

        let r = NarReceiver::with_partial_store(store);
        r.note_header("j1", &path, 6, "t1");
        r.note_header("j2", &path, 6, "t2");
        let p1 = r.register("j1", &path);
        let p2 = r.register("j2", &path);

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

    #[tokio::test]
    async fn a_restart_from_zero_drops_the_resumed_prefix() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
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
    async fn dropping_the_receiver_ends_every_stager() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
        let hash = "dddddddddddddddddddddddddddddddd";
        let path = format!("/nix/store/{hash}-pkg");

        let r = NarReceiver::with_partial_store(store.clone());
        let stagers = r.stagers.clone();
        let _pending = r.register("j", &path);
        r.note_header("j", &path, 9, "len-9");
        r.accept_chunk("j", &path, frame("j", &path, 0, b"abc", false))
            .await;

        drop(r);
        stagers.close();
        tokio::time::timeout(Duration::from_secs(5), stagers.wait())
            .await
            .expect("a stager must end when the last receiver is dropped");

        assert_eq!(store.staged_len(&format!("j/{hash}")).await, 3);
    }

    #[tokio::test]
    async fn a_chunk_nothing_requested_is_not_staged() {
        let dir = TempDir::new().unwrap();
        let store = PartialStore::new(dir.path()).unwrap();
        let hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let path = format!("/nix/store/{hash}-pkg");

        let r = NarReceiver::with_partial_store(store.clone());
        r.accept_chunk("gone", &path, frame("gone", &path, 0, b"abc", false))
            .await;

        assert!(r.inner.lock().streams.is_empty());
        assert_eq!(store.staged_len(&format!("gone/{hash}")).await, 0);
    }

    #[tokio::test]
    async fn absorb_feeds_a_pull_stream_into_its_waiter() {
        let r = NarReceiver::new();
        let pending = r.register("j", "/nix/store/p");
        let header = ServerMessage::NarStreamHeader {
            job_id: "j".into(),
            store_path: "/nix/store/p".into(),
            total_bytes: 3,
            stream_token: "t".into(),
        };
        assert!(r.absorb(Inbound::Control(header)).await.is_none());

        for chunk in [
            frame("j", "/nix/store/p", 0, b"abc", false),
            frame("j", "/nix/store/p", 3, b"", true),
        ] {
            assert!(r.absorb(Inbound::Bulk(chunk)).await.is_none());
        }

        assert_eq!(bytes(r.await_pending(pending).await.unwrap()), b"abc");
    }

    #[tokio::test]
    async fn absorb_types_an_unavailable_nar_and_an_abort_apart() {
        let r = NarReceiver::new();
        let gone = r.register("j", "/nix/store/gone");
        let dropped = r.register("j", "/nix/store/dropped");
        let unavailable = ServerMessage::NarUnavailable {
            job_id: "j".into(),
            store_path: "/nix/store/gone".into(),
            reason: "missing".into(),
        };
        let abort = ServerMessage::NarAbort {
            job_id: "j".into(),
            store_path: "/nix/store/dropped".into(),
            reason: "reset".into(),
        };
        assert!(r.absorb(Inbound::Control(unavailable)).await.is_none());
        assert!(r.absorb(Inbound::Control(abort)).await.is_none());

        let gone = r.await_pending(gone).await.unwrap_err();
        assert!(gone.downcast_ref::<NarUnavailable>().is_some());
        let dropped = r.await_pending(dropped).await.unwrap_err();
        assert!(dropped.downcast_ref::<NarUnavailable>().is_none());
    }

    #[tokio::test]
    async fn absorb_hands_back_what_is_not_a_nar_transfer() {
        let r = NarReceiver::new();
        let back = r.absorb(Inbound::Control(ServerMessage::Draining)).await;
        assert!(matches!(
            back,
            Some(Inbound::Control(ServerMessage::Draining))
        ));
    }
}
