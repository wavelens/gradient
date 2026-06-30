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
//! [`NarReceiver::note_header`] and calls [`NarReceiver::accept_chunk`] for
//! each arriving `ServerMessage::NarPush`. When a [`gradient_storage::PartialStore`]
//! is configured, chunks are staged to disk keyed by the NAR hash so an
//! interrupted download can resume (issue #225); otherwise they accumulate in
//! memory (used by tests). On `is_final` the assembled buffer is delivered to
//! the waiting task via a `oneshot`.
//!
//! `NarUnavailable` / `NarAbort` are routed through [`NarReceiver::fail`] so
//! the waiter resolves with the reason immediately. The on-disk partial is
//! kept on failure so the next request can resume from where it stopped.
//!
//! For uploads, [`NarReceiver::register_push`] installs a one-shot gate that
//! the dispatch loop resolves on
//! [`gradient_proto::messages::ServerMessage::NarPushResume`], handing the
//! pusher the byte offset to seek to.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;
use tracing::{debug, warn};

/// Hard ceiling on a single NAR transfer. Hit only when the server stops
/// responding entirely without sending `NarUnavailable`/`NarAbort`.
const NAR_RECV_TIMEOUT: Duration = Duration::from_secs(600);

/// Ceiling on the push-resume handshake. A server that never answers a
/// `NarStreamHeader` falls back to a fresh upload from offset 0.
const PUSH_RESUME_TIMEOUT: Duration = Duration::from_secs(30);

type Key = (String, String); // (job_id, store_path)

/// Metadata from a `NarStreamHeader` preceding the chunks for a pull.
struct HeaderInfo {
    total_bytes: u64,
    token: String,
}

#[derive(Default)]
struct Inner {
    /// In-memory NAR bytes, used only when no `PartialStore` is configured.
    buffers: HashMap<Key, Vec<u8>>,
    /// Server-advertised size + token for the in-flight pull.
    headers: HashMap<Key, HeaderInfo>,
    /// Outstanding pull waiters; resolved on `is_final` or on failure.
    waiters: HashMap<Key, oneshot::Sender<Result<Vec<u8>, String>>>,
    /// First-chunk arrival time per key for the throughput EWMA.
    started: HashMap<Key, std::time::Instant>,
    /// Outstanding push-resume gates; resolved on `NarPushResume`.
    push_waiters: HashMap<Key, oneshot::Sender<u64>>,
}

/// Shared state between the dispatch loop and job tasks for routing inbound
/// NARs and resolving push-resume handshakes.
#[derive(Clone, Default)]
pub struct NarReceiver {
    inner: Arc<Mutex<Inner>>,
    /// When set, pull chunks are staged to disk (keyed by NAR hash) so an
    /// interrupted download survives a reconnect. `None` keeps everything in
    /// memory (tests).
    partial: Option<gradient_storage::PartialStore>,
}

/// Outstanding pull waiter handle returned by [`NarReceiver::register`].
pub struct PendingNar {
    job_id: String,
    store_path: String,
    rx: oneshot::Receiver<Result<Vec<u8>, String>>,
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

impl NarReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Receiver that stages pull chunks to `store` for resumable downloads.
    pub fn with_partial_store(store: gradient_storage::PartialStore) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            partial: Some(store),
        }
    }

    /// Bytes already staged on disk for `store_path` and the token they were
    /// received under, if any. Returns `(0, None)` in memory-only mode or when
    /// nothing is staged - used by the requester to decide between
    /// `NarRequest` and `NarRequestResume`.
    pub fn resumable(&self, job_id: &str, store_path: &str) -> (u64, Option<String>) {
        let Some(store) = self.partial.as_ref() else {
            return (0, None);
        };
        let Some(key) = partial_key(job_id, store_path) else {
            return (0, None);
        };
        (store.staged_len(&key), store.token(&key))
    }

    /// Synchronously install a waiter for `(job_id, store_path)`.
    pub fn register(&self, job_id: &str, store_path: &str) -> PendingNar {
        let key = (job_id.to_owned(), store_path.to_owned());
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap().waiters.insert(key, tx);
        PendingNar {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            rx,
        }
    }

    /// Await a previously [`Self::register`]ed waiter, bounded by
    /// [`NAR_RECV_TIMEOUT`].
    pub async fn await_pending(&self, pending: PendingNar) -> Result<Vec<u8>> {
        let PendingNar {
            job_id,
            store_path,
            rx,
        } = pending;
        let key = (job_id.clone(), store_path.clone());
        match tokio::time::timeout(NAR_RECV_TIMEOUT, rx).await {
            Ok(Ok(Ok(bytes))) => Ok(bytes),
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
                let mut g = self.inner.lock().unwrap();
                g.buffers.remove(&key);
                g.headers.remove(&key);
                g.waiters.remove(&key);
                g.started.remove(&key);
                Err(anyhow::anyhow!(
                    "NarRequest for {} timed out after {}s waiting for NarPush \
                     (job_id={})",
                    store_path,
                    NAR_RECV_TIMEOUT.as_secs(),
                    job_id,
                ))
            }
        }
    }

    /// Convenience: register + await in one step.
    #[cfg(test)]
    pub async fn wait_for(&self, job_id: &str, store_path: &str) -> Result<Vec<u8>> {
        let pending = self.register(job_id, store_path);
        self.await_pending(pending).await
    }

    /// Record the `NarStreamHeader` that precedes a pull's chunks.
    pub fn note_header(&self, job_id: &str, store_path: &str, total_bytes: u64, token: &str) {
        let key = (job_id.to_owned(), store_path.to_owned());
        self.inner.lock().unwrap().headers.insert(
            key,
            HeaderInfo {
                total_bytes,
                token: token.to_owned(),
            },
        );
    }

    /// Append a `NarPush` chunk at `offset`. When `is_final` is true the
    /// assembled buffer is delivered to any registered waiter.
    pub async fn accept_chunk(
        &self,
        job_id: &str,
        store_path: &str,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    ) {
        let key = (job_id.to_owned(), store_path.to_owned());

        // Snapshot the active token and stamp the start time without holding
        // the lock across disk I/O.
        let token = {
            let mut g = self.inner.lock().unwrap();
            if !data.is_empty() {
                g.started.entry(key.clone()).or_insert_with(std::time::Instant::now);
            }
            g.headers.get(&key).map(|h| h.token.clone()).unwrap_or_default()
        };

        if !data.is_empty() {
            match (self.partial.clone(), partial_key(job_id, store_path)) {
                (Some(store), Some(pkey)) => {
                    // Disk staging is blocking `std::fs`; run it on the blocking
                    // pool so the dispatch task is never stalled mid-pull.
                    let outcome = tokio::task::spawn_blocking(move || {
                        match store.append(&pkey, &token, offset, &data) {
                            Ok(()) => Ok(()),
                            // Non-fatal: drop the partial so a retry restarts cleanly.
                            Err(e) => {
                                let _ = store.discard(&pkey);
                                Err(format!("partial append failed: {e}"))
                            }
                        }
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("partial append task panicked: {e}")));
                    if let Err(msg) = outcome {
                        self.deliver(&key, Err(msg));
                        return;
                    }
                }
                _ => {
                    self.inner
                        .lock()
                        .unwrap()
                        .buffers
                        .entry(key.clone())
                        .or_default()
                        .extend_from_slice(&data);
                }
            }
        }

        if !is_final {
            return;
        }

        // Take the in-memory state under the lock; read/discard the on-disk
        // partial off the runtime.
        let (expected, started, disk, mem_buf) = {
            let mut g = self.inner.lock().unwrap();
            let expected = g.headers.remove(&key).map(|h| h.total_bytes);
            let started = g.started.remove(&key);
            let disk = match (self.partial.clone(), partial_key(job_id, store_path)) {
                (Some(store), Some(pkey)) => Some((store, pkey)),
                _ => None,
            };
            let mem_buf = match disk {
                Some(_) => None,
                None => Some(g.buffers.remove(&key).unwrap_or_default()),
            };
            (expected, started, disk, mem_buf)
        };

        let buf = match (mem_buf, disk) {
            (Some(b), _) => b,
            (None, Some((store, pkey))) => tokio::task::spawn_blocking(move || {
                let b = store.read_all(&pkey).unwrap_or_default();
                let _ = store.discard(&pkey);
                b
            })
            .await
            .unwrap_or_default(),
            (None, None) => Vec::new(),
        };

        if let Some(start) = started {
            crate::metrics::throughput::NETWORK.observe(
                buf.len() as f64 * 8.0 / start.elapsed().as_secs_f64().max(1e-6) / 1_000_000.0,
            );
        }

        if let Some(total) = expected
            && buf.len() as u64 != total
        {
            self.deliver(
                &key,
                Err(format!(
                    "assembled NAR {} bytes != advertised {} bytes",
                    buf.len(),
                    total
                )),
            );
            return;
        }

        self.deliver(&key, Ok(buf));
    }

    /// Resolve the waiter for `key`, warning if none is registered.
    fn deliver(&self, key: &Key, result: Result<Vec<u8>, String>) {
        let mut g = self.inner.lock().unwrap();
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
        let mut g = self.inner.lock().unwrap();
        g.buffers.remove(&key);
        g.headers.remove(&key);
        g.started.remove(&key);
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
        self.inner.lock().unwrap().push_waiters.insert(key, tx);
        PushResumeGate { rx }
    }

    /// Resolve a push-resume gate with the server's `received_bytes`.
    pub fn resolve_push(&self, job_id: &str, store_path: &str, received_bytes: u64) {
        let key = (job_id.to_owned(), store_path.to_owned());
        if let Some(tx) = self.inner.lock().unwrap().push_waiters.remove(&key) {
            let _ = tx.send(received_bytes);
        } else {
            debug!(%job_id, %store_path, "NarPushResume with no push gate - discarding");
        }
    }

    /// Drop in-memory state for a job. On-disk partials (keyed by hash) are
    /// left for the GC sweep so a later attempt can still resume.
    pub fn forget_job(&self, job_id: &str) {
        let mut g = self.inner.lock().unwrap();
        g.buffers.retain(|(j, _), _| j != job_id);
        g.headers.retain(|(j, _), _| j != job_id);
        g.waiters.retain(|(j, _), _| j != job_id);
        g.started.retain(|(j, _), _| j != job_id);
        g.push_waiters.retain(|(j, _), _| j != job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn final_chunk(r: &NarReceiver, job: &str, path: &str, data: &[u8]) {
        r.accept_chunk(job, path, data.to_vec(), 0, true).await;
    }

    #[tokio::test]
    async fn single_chunk_delivers_to_waiter() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("job1", "/nix/store/aaa").await });
        tokio::task::yield_now().await;
        final_chunk(&r, "job1", "/nix/store/aaa", b"hello world").await;
        let bytes = task.await.unwrap().unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn multi_chunk_assembled_in_order() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.accept_chunk("j", "/nix/store/x", b"abc".to_vec(), 0, false).await;
        r.accept_chunk("j", "/nix/store/x", b"def".to_vec(), 3, false).await;
        r.accept_chunk("j", "/nix/store/x", b"ghi".to_vec(), 6, true).await;
        let bytes = task.await.unwrap().unwrap();
        assert_eq!(bytes, b"abcdefghi");
    }

    #[tokio::test]
    async fn final_with_no_waiter_is_discarded() {
        let r = NarReceiver::new();
        final_chunk(&r, "j", "/nix/store/x", b"orphan").await;
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        final_chunk(&r, "j", "/nix/store/x", b"second").await;
        assert_eq!(task.await.unwrap().unwrap(), b"second");
    }

    #[tokio::test]
    async fn forget_job_cancels_waiters() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("doomed", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.forget_job("doomed");
        assert!(task.await.unwrap().is_err(), "waiter should have been cancelled");
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
        assert_eq!(r.await_pending(p2).await.unwrap(), b"hello");
    }

    /// A `PartialStore`-backed receiver resumes across a simulated reconnect:
    /// the first attempt stages bytes to disk and fails; a fresh receiver over
    /// the same root reports the staged prefix and completes the transfer.
    #[tokio::test]
    async fn partial_store_resumes_across_reconnect() {
        let dir = TempDir::new().unwrap();
        let store = gradient_storage::PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let path = format!("/nix/store/{hash}-pkg");

        let r1 = NarReceiver::with_partial_store(store.clone());
        r1.note_header("j", &path, 9, "len-9");
        r1.accept_chunk("j", &path, b"abc".to_vec(), 0, false).await;
        r1.accept_chunk("j", &path, b"def".to_vec(), 3, false).await;
        // Connection drops mid-transfer.
        r1.fail("j", &path, "NarAbort".into());

        let (staged, token) = r1.resumable("j", &path);
        assert_eq!(staged, 6);
        assert_eq!(token.as_deref(), Some("len-9"));

        // Fresh receiver (reconnect) resumes from offset 6 and completes.
        let r2 = NarReceiver::with_partial_store(store);
        let r2c = r2.clone();
        let pathc = path.clone();
        let task = tokio::spawn(async move { r2c.wait_for("j", &pathc).await });
        tokio::task::yield_now().await;
        r2.note_header("j", &path, 9, "len-9");
        r2.accept_chunk("j", &path, b"ghi".to_vec(), 6, true).await;
        assert_eq!(task.await.unwrap().unwrap(), b"abcdefghi");
    }

    /// Two jobs transferring the SAME store path concurrently must not share a
    /// partial file. With a bare-hash key their interleaved appends corrupted
    /// the partial and failed "non-contiguous"; per-`{job_id}/{hash}` keys keep
    /// them isolated so both assemble correctly.
    #[tokio::test]
    async fn concurrent_jobs_same_path_do_not_collide() {
        let dir = TempDir::new().unwrap();
        let store =
            gradient_storage::PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
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
        r.accept_chunk("j1", &path, b"aaa".to_vec(), 0, false).await;
        r.accept_chunk("j2", &path, b"bbb".to_vec(), 0, false).await;
        r.accept_chunk("j1", &path, b"AAA".to_vec(), 3, true).await;
        r.accept_chunk("j2", &path, b"BBB".to_vec(), 3, true).await;

        assert_eq!(r.await_pending(p1).await.unwrap(), b"aaaAAA");
        assert_eq!(r.await_pending(p2).await.unwrap(), b"bbbBBB");
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
