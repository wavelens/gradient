/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Routing layer for the fleet eval-cache transfer (issue #386 L3).
//!
//! Before evaluating a flake the worker PULLs its `<fingerprint>.sqlite` blob
//! from the server so the eval runs warm; after, it PUSHes the updated blob
//! back. Both legs are best-effort - a cache failure never fails the eval.
//!
//! [`super::job::JobUpdater::pull_eval_cache`] registers a [`PendingPull`] then
//! sends `EvalCachePull`; the dispatch loop routes `EvalCachePullResult` via
//! [`EvalCacheReceiver::deliver_pull_result`] and inline-stream `EvalCacheChunk`
//! frames via [`EvalCacheReceiver::deliver_pull_chunk`]. The push leg registers
//! a [`PendingPush`] then sends `EvalCachePush`; the grant is routed via
//! [`EvalCacheReceiver::deliver_push_grant`].
//!
//! Mirrors [`super::nar_recv`] but simpler: the executor runs one eval at a
//! time so a single in-flight pull *or* push per `job_id` is enough.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use gradient_proto::messages::{EvalCachePullOutcome, EvalCachePushMode};
use tokio::sync::oneshot;
use tracing::{debug, warn};

/// Ceiling on a single eval-cache transfer (control message + inline chunk
/// stream). Matches the NAR pull ceiling - the blob travels the same channel.
const EVAL_CACHE_TIMEOUT: Duration = Duration::from_secs(600);

/// Per-job in-flight transfer state. The executor is sequential so at most one
/// pull or push is live for a given `job_id` at a time.
enum Pending {
    /// Awaiting `EvalCachePullResult`; on `Inline` it transitions to `PullStream`.
    Pull {
        result_tx: oneshot::Sender<EvalCachePullOutcome>,
    },
    /// Accumulating inline `EvalCacheChunk` frames after an `Inline` outcome.
    PullStream {
        buf: Vec<u8>,
        bytes_tx: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// Awaiting `EvalCachePushGrant`.
    Push {
        grant_tx: oneshot::Sender<EvalCachePushMode>,
    },
}

#[derive(Default)]
struct Inner {
    pending: HashMap<String, Pending>,
}

/// Shared state between the dispatch loop and job tasks for routing eval-cache
/// transfers. Cloneable; cheap.
#[derive(Clone, Default)]
pub struct EvalCacheReceiver {
    inner: Arc<Mutex<Inner>>,
}

/// Pull handle returned by [`EvalCacheReceiver::register_pull`].
pub struct PendingPull {
    job_id: String,
    result_rx: oneshot::Receiver<EvalCachePullOutcome>,
    recv: EvalCacheReceiver,
}

/// Push handle returned by [`EvalCacheReceiver::register_push`].
pub struct PendingPush {
    job_id: String,
    grant_rx: oneshot::Receiver<EvalCachePushMode>,
    recv: EvalCacheReceiver,
}

impl PendingPull {
    /// Await the `EvalCachePullResult` outcome, bounded by [`EVAL_CACHE_TIMEOUT`].
    pub async fn await_outcome(&mut self) -> Result<EvalCachePullOutcome> {
        match tokio::time::timeout(EVAL_CACHE_TIMEOUT, &mut self.result_rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "eval-cache pull waiter dropped (job_id={}) - connection closed?",
                self.job_id
            )),
            Err(_) => {
                self.recv.forget_job(&self.job_id);
                Err(anyhow::anyhow!(
                    "eval-cache pull for job_id={} timed out after {}s",
                    self.job_id,
                    EVAL_CACHE_TIMEOUT.as_secs(),
                ))
            }
        }
    }

    /// After an `Inline` outcome, switch this handle into chunk-accumulation
    /// mode and await the assembled blob delivered on `is_final`.
    pub async fn await_inline(self, total_bytes: u64) -> Result<Vec<u8>> {
        let (bytes_tx, bytes_rx) = oneshot::channel();
        self.recv.inner.lock().unwrap().pending.insert(
            self.job_id.clone(),
            Pending::PullStream {
                buf: Vec::with_capacity(total_bytes as usize),
                bytes_tx,
            },
        );

        match tokio::time::timeout(EVAL_CACHE_TIMEOUT, bytes_rx).await {
            Ok(Ok(Ok(bytes))) => {
                if bytes.len() as u64 != total_bytes {
                    return Err(anyhow::anyhow!(
                        "assembled eval-cache blob {} bytes != advertised {}",
                        bytes.len(),
                        total_bytes
                    ));
                }

                Ok(bytes)
            }
            Ok(Ok(Err(reason))) => Err(anyhow::anyhow!("eval-cache inline pull failed: {reason}")),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "eval-cache inline waiter dropped (job_id={})",
                self.job_id
            )),
            Err(_) => {
                self.recv.forget_job(&self.job_id);
                Err(anyhow::anyhow!(
                    "eval-cache inline pull for job_id={} timed out after {}s",
                    self.job_id,
                    EVAL_CACHE_TIMEOUT.as_secs(),
                ))
            }
        }
    }
}

impl PendingPush {
    /// Await the `EvalCachePushGrant` mode, bounded by [`EVAL_CACHE_TIMEOUT`].
    pub async fn await_grant(&mut self) -> Result<EvalCachePushMode> {
        match tokio::time::timeout(EVAL_CACHE_TIMEOUT, &mut self.grant_rx).await {
            Ok(Ok(mode)) => Ok(mode),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "eval-cache push waiter dropped (job_id={}) - connection closed?",
                self.job_id
            )),
            Err(_) => {
                self.recv.forget_job(&self.job_id);
                Err(anyhow::anyhow!(
                    "eval-cache push for job_id={} timed out after {}s",
                    self.job_id,
                    EVAL_CACHE_TIMEOUT.as_secs(),
                ))
            }
        }
    }
}

impl EvalCacheReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a pull waiter for `job_id` before sending `EvalCachePull`.
    pub fn register_pull(&self, job_id: &str) -> PendingPull {
        let (result_tx, result_rx) = oneshot::channel();
        self.inner
            .lock()
            .unwrap()
            .pending
            .insert(job_id.to_owned(), Pending::Pull { result_tx });
        PendingPull {
            job_id: job_id.to_owned(),
            result_rx,
            recv: self.clone(),
        }
    }

    /// Install a push waiter for `job_id` before sending `EvalCachePush`.
    pub fn register_push(&self, job_id: &str) -> PendingPush {
        let (grant_tx, grant_rx) = oneshot::channel();
        self.inner
            .lock()
            .unwrap()
            .pending
            .insert(job_id.to_owned(), Pending::Push { grant_tx });
        PendingPush {
            job_id: job_id.to_owned(),
            grant_rx,
            recv: self.clone(),
        }
    }

    /// Route an `EvalCachePullResult` to its waiter.
    pub fn deliver_pull_result(&self, job_id: &str, outcome: EvalCachePullOutcome) {
        let pending = self.inner.lock().unwrap().pending.remove(job_id);
        match pending {
            Some(Pending::Pull { result_tx }) => {
                if result_tx.send(outcome).is_err() {
                    debug!(%job_id, "eval-cache pull waiter went away before delivery");
                }
            }
            _ => warn!(%job_id, "EvalCachePullResult with no pull waiter - discarding"),
        }
    }

    /// Append an inline `EvalCacheChunk`; on `is_final` deliver the assembled
    /// blob. A non-contiguous offset fails the waiter, mirroring `nar_recv`.
    pub fn deliver_pull_chunk(&self, job_id: &str, data: Vec<u8>, offset: u64, is_final: bool) {
        let mut g = self.inner.lock().unwrap();
        let Some(Pending::PullStream { buf, .. }) = g.pending.get_mut(job_id) else {
            warn!(%job_id, "EvalCacheChunk with no inline pull stream - discarding");
            return;
        };

        if offset != buf.len() as u64 {
            let expected = buf.len() as u64;
            if let Some(Pending::PullStream { bytes_tx, .. }) = g.pending.remove(job_id)
                && bytes_tx
                    .send(Err(format!(
                        "non-contiguous eval-cache chunk: offset {offset} != expected {expected}"
                    )))
                    .is_err()
            {
                debug!(%job_id, "eval-cache inline waiter went away before error delivery");
            }

            return;
        }

        buf.extend_from_slice(&data);

        if is_final
            && let Some(Pending::PullStream { buf, bytes_tx }) = g.pending.remove(job_id)
            && bytes_tx.send(Ok(buf)).is_err()
        {
            debug!(%job_id, "eval-cache inline waiter went away before delivery");
        }
    }

    /// Route an `EvalCachePushGrant` to its waiter.
    pub fn deliver_push_grant(&self, job_id: &str, mode: EvalCachePushMode) {
        let pending = self.inner.lock().unwrap().pending.remove(job_id);
        match pending {
            Some(Pending::Push { grant_tx }) => {
                if grant_tx.send(mode).is_err() {
                    debug!(%job_id, "eval-cache push waiter went away before delivery");
                }
            }
            _ => warn!(%job_id, "EvalCachePushGrant with no push waiter - discarding"),
        }
    }

    /// Drop any pending transfer state for `job_id`. Called from job cleanup
    /// next to `nar_recv.forget_job`.
    pub fn forget_job(&self, job_id: &str) {
        self.inner.lock().unwrap().pending.remove(job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inline_chunks_assemble_in_order() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result(
            "j",
            EvalCachePullOutcome::Inline {
                total_bytes: 9,
                stream_token: "t".into(),
            },
        );
        let outcome = pull.await_outcome().await.unwrap();
        let total = match outcome {
            EvalCachePullOutcome::Inline { total_bytes, .. } => total_bytes,
            other => panic!("expected Inline, got {other:?}"),
        };

        let r2 = r.clone();
        let task = tokio::spawn(async move { pull.await_inline(total).await });
        tokio::task::yield_now().await;
        r2.deliver_pull_chunk("j", b"abc".to_vec(), 0, false);
        r2.deliver_pull_chunk("j", b"def".to_vec(), 3, false);
        r2.deliver_pull_chunk("j", b"ghi".to_vec(), 6, true);
        assert_eq!(task.await.unwrap().unwrap(), b"abcdefghi");
    }

    #[tokio::test]
    async fn single_final_chunk_delivers() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result(
            "j",
            EvalCachePullOutcome::Inline {
                total_bytes: 5,
                stream_token: "t".into(),
            },
        );
        pull.await_outcome().await.unwrap();

        let r2 = r.clone();
        let task = tokio::spawn(async move { pull.await_inline(5).await });
        tokio::task::yield_now().await;
        r2.deliver_pull_chunk("j", b"hello".to_vec(), 0, true);
        assert_eq!(task.await.unwrap().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn non_contiguous_chunk_fails_waiter() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result(
            "j",
            EvalCachePullOutcome::Inline {
                total_bytes: 6,
                stream_token: "t".into(),
            },
        );
        pull.await_outcome().await.unwrap();

        let r2 = r.clone();
        let task = tokio::spawn(async move { pull.await_inline(6).await });
        tokio::task::yield_now().await;
        r2.deliver_pull_chunk("j", b"abc".to_vec(), 0, false);
        r2.deliver_pull_chunk("j", b"def".to_vec(), 99, true);
        let err = task.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("non-contiguous"), "got: {err}");
    }

    #[tokio::test]
    async fn size_mismatch_fails() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result(
            "j",
            EvalCachePullOutcome::Inline {
                total_bytes: 10,
                stream_token: "t".into(),
            },
        );
        pull.await_outcome().await.unwrap();

        let r2 = r.clone();
        let task = tokio::spawn(async move { pull.await_inline(10).await });
        tokio::task::yield_now().await;
        r2.deliver_pull_chunk("j", b"short".to_vec(), 0, true);
        let err = task.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("!= advertised"), "got: {err}");
    }

    #[tokio::test]
    async fn pull_result_routes_miss() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result("j", EvalCachePullOutcome::Miss);
        assert!(matches!(
            pull.await_outcome().await.unwrap(),
            EvalCachePullOutcome::Miss
        ));
    }

    #[tokio::test]
    async fn push_grant_routes() {
        let r = EvalCacheReceiver::new();
        let mut push = r.register_push("j");
        r.deliver_push_grant("j", EvalCachePushMode::Skip);
        assert!(matches!(
            push.await_grant().await.unwrap(),
            EvalCachePushMode::Skip
        ));
    }

    #[tokio::test]
    async fn forget_job_cancels_pull_waiter() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("doomed");
        r.forget_job("doomed");
        assert!(pull.await_outcome().await.is_err());
    }
}
