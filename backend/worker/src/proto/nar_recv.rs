/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Routing layer for incoming NAR transfers (server → worker).
//!
//! When a job task sends `NarRequest { paths }` it then calls
//! [`NarReceiver::wait_for`] to await the assembled compressed NAR for each
//! path. The dispatch loop calls [`NarReceiver::accept_chunk`] for each
//! arriving `ServerMessage::NarPush`; chunks are accumulated per
//! `(job_id, store_path)` and on `is_final` the buffer is delivered to the
//! waiting task via a `oneshot`.
//!
//! When the server can't serve a requested NAR it emits
//! [`proto::messages::ServerMessage::NarUnavailable`] before any chunk, or
//! [`proto::messages::ServerMessage::NarAbort`] mid-stream. Both are routed
//! through [`NarReceiver::fail`] so the waiter resolves with the reason
//! immediately instead of timing out 600 s later.
//!
//! Keyed by `(job_id, store_path)` so multiple concurrent jobs can request the
//! same path without collision.
//!
//! No I/O happens here — decompression and store import are the caller's job.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;
use tracing::{debug, warn};

/// Hard ceiling on a single NAR transfer. Hit only when the server stops
/// responding entirely without sending `NarUnavailable`/`NarAbort` (e.g.
/// connection silently stalled). Normal failures resolve immediately via
/// [`NarReceiver::fail`].
const NAR_RECV_TIMEOUT: Duration = Duration::from_secs(600);

type Key = (String, String); // (job_id, store_path)

#[derive(Default)]
struct Inner {
    /// Partial NAR bytes accumulated from `NarPush` chunks.
    buffers: HashMap<Key, Vec<u8>>,
    /// Outstanding waiters; the dispatch loop sends to these on `is_final`
    /// (success) or on `NarUnavailable` / `NarAbort` (failure).
    waiters: HashMap<Key, oneshot::Sender<Result<Vec<u8>, String>>>,
}

/// Shared state between the dispatch loop and job tasks for routing inbound
/// NARs.
#[derive(Clone, Default)]
pub struct NarReceiver {
    inner: Arc<Mutex<Inner>>,
}

impl NarReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a waiter for `(job_id, store_path)`. The returned oneshot
    /// resolves with the assembled NAR bytes on `is_final`, or with an error
    /// if the server signals `NarUnavailable` / `NarAbort`. Wraps the wait
    /// in [`NAR_RECV_TIMEOUT`] as a last-resort backstop.
    pub async fn wait_for(&self, job_id: &str, store_path: &str) -> Result<Vec<u8>> {
        let key = (job_id.to_owned(), store_path.to_owned());
        let (tx, rx) = oneshot::channel();
        // Replacing an existing waiter for the same key drops the old sender;
        // its receiver will see `RecvError` and bail out instead of hanging.
        self.inner.lock().unwrap().waiters.insert(key.clone(), tx);

        match tokio::time::timeout(NAR_RECV_TIMEOUT, rx).await {
            Ok(Ok(Ok(bytes))) => Ok(bytes),
            Ok(Ok(Err(reason))) => {
                // Server-signalled failure. Buffer was already cleared in
                // `fail()`.
                Err(anyhow::anyhow!(
                    "NAR transfer for {} failed: {}",
                    store_path,
                    reason
                ))
            }
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "NarPush waiter dropped for {}/{} — connection closed or superseded?",
                job_id,
                store_path
            )),
            Err(_) => {
                // Drop the partial buffer and waiter so a late chunk doesn't
                // try to deliver to a closed channel.
                let mut g = self.inner.lock().unwrap();
                g.buffers.remove(&key);
                g.waiters.remove(&key);
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

    /// Append a `NarPush` chunk. When `is_final` is true the assembled buffer
    /// is delivered to any registered waiter (or dropped + warned if none).
    pub fn accept_chunk(&self, job_id: &str, store_path: &str, data: Vec<u8>, is_final: bool) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let mut g = self.inner.lock().unwrap();
        if !data.is_empty() {
            g.buffers
                .entry(key.clone())
                .or_default()
                .extend_from_slice(&data);
        }
        if is_final {
            let buf = g.buffers.remove(&key).unwrap_or_default();
            match g.waiters.remove(&key) {
                Some(tx) => {
                    let bytes_len = buf.len();
                    if tx.send(Ok(buf)).is_err() {
                        debug!(
                            %job_id,
                            %store_path,
                            bytes = bytes_len,
                            "NarPush waiter went away before delivery"
                        );
                    }
                }
                None => {
                    warn!(
                        %job_id,
                        %store_path,
                        bytes = buf.len(),
                        "received final NarPush with no waiter — discarding"
                    );
                }
            }
        }
    }

    /// Resolve the waiter for `(job_id, store_path)` with an error and drop
    /// any partial buffer. Called for both `NarUnavailable` (transfer never
    /// started) and `NarAbort` (transfer aborted mid-stream).
    pub fn fail(&self, job_id: &str, store_path: &str, reason: String) {
        let key = (job_id.to_owned(), store_path.to_owned());
        let mut g = self.inner.lock().unwrap();
        g.buffers.remove(&key);
        match g.waiters.remove(&key) {
            Some(tx) => {
                if tx.send(Err(reason)).is_err() {
                    debug!(
                        %job_id,
                        %store_path,
                        "NAR failure waiter went away before delivery"
                    );
                }
            }
            None => {
                warn!(
                    %job_id,
                    %store_path,
                    %reason,
                    "received NarUnavailable/NarAbort with no waiter — discarding"
                );
            }
        }
    }

    /// Drop every buffer and waiter associated with a job. Called from the
    /// dispatch loop when a job ends so we don't leak partial buffers if the
    /// task aborted mid-fetch.
    pub fn forget_job(&self, job_id: &str) {
        let mut g = self.inner.lock().unwrap();
        g.buffers.retain(|(j, _), _| j != job_id);
        g.waiters.retain(|(j, _), _| j != job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_chunk_delivers_to_waiter() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("job1", "/nix/store/aaa").await });
        tokio::task::yield_now().await;
        r.accept_chunk("job1", "/nix/store/aaa", b"hello world".to_vec(), true);
        let bytes = task.await.unwrap().unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn multi_chunk_assembled_in_order() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.accept_chunk("j", "/nix/store/x", b"abc".to_vec(), false);
        r.accept_chunk("j", "/nix/store/x", b"def".to_vec(), false);
        r.accept_chunk("j", "/nix/store/x", b"ghi".to_vec(), true);
        let bytes = task.await.unwrap().unwrap();
        assert_eq!(bytes, b"abcdefghi");
    }

    #[tokio::test]
    async fn final_with_no_waiter_is_discarded() {
        let r = NarReceiver::new();
        r.accept_chunk("j", "/nix/store/x", b"orphan".to_vec(), true);
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.accept_chunk("j", "/nix/store/x", b"second".to_vec(), true);
        assert_eq!(task.await.unwrap().unwrap(), b"second");
    }

    #[tokio::test]
    async fn forget_job_cancels_waiters() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        let task = tokio::spawn(async move { r2.wait_for("doomed", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.forget_job("doomed");
        let result = task.await.unwrap();
        assert!(result.is_err(), "waiter should have been cancelled");
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
    async fn fail_clears_partial_buffer() {
        let r = NarReceiver::new();
        let r2 = r.clone();
        // Accumulate a partial buffer, then abort, then start a fresh request.
        r.accept_chunk("j", "/nix/store/x", b"partial".to_vec(), false);
        r.fail("j", "/nix/store/x", "mid-stream abort".into());
        // A subsequent transfer must not see the discarded prefix.
        let task = tokio::spawn(async move { r2.wait_for("j", "/nix/store/x").await });
        tokio::task::yield_now().await;
        r.accept_chunk("j", "/nix/store/x", b"fresh".to_vec(), true);
        assert_eq!(task.await.unwrap().unwrap(), b"fresh");
    }
}
