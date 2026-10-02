/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use gradient_wire::messages::{EvalCachePullOutcome, TRANSFER_TIMEOUT};
use tokio::sync::oneshot;
use tracing::{debug, warn};

enum Pending {
    /// The dispatch loop is moving this to `PullStream` on `Inline` before waking the waiter. The
    /// chunks are following the result immediately.
    Pull {
        result_tx: oneshot::Sender<EvalCachePullOutcome>,
        bytes_tx: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    PullStream {
        buf: Vec<u8>,
        bytes_tx: oneshot::Sender<Result<Vec<u8>, String>>,
    },
}

#[derive(Default)]
struct Inner {
    pending: HashMap<String, Pending>,
}

#[derive(Clone, Default)]
pub struct EvalCacheReceiver {
    inner: Arc<Mutex<Inner>>,
}

pub struct PendingPull {
    job_id: String,
    result_rx: oneshot::Receiver<EvalCachePullOutcome>,
    bytes_rx: oneshot::Receiver<Result<Vec<u8>, String>>,
    recv: EvalCacheReceiver,
}

impl PendingPull {
    pub async fn await_outcome(&mut self) -> Result<EvalCachePullOutcome> {
        match tokio::time::timeout(TRANSFER_TIMEOUT, &mut self.result_rx).await {
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
                    TRANSFER_TIMEOUT.as_secs(),
                ))
            }
        }
    }

    pub async fn await_inline(self, total_bytes: u64) -> Result<Vec<u8>> {
        match tokio::time::timeout(TRANSFER_TIMEOUT, self.bytes_rx).await {
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
                    TRANSFER_TIMEOUT.as_secs(),
                ))
            }
        }
    }
}

impl EvalCacheReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_pull(&self, job_id: &str) -> PendingPull {
        let (result_tx, result_rx) = oneshot::channel();
        let (bytes_tx, bytes_rx) = oneshot::channel();
        self.inner.lock().pending.insert(
            job_id.to_owned(),
            Pending::Pull {
                result_tx,
                bytes_tx,
            },
        );
        PendingPull {
            job_id: job_id.to_owned(),
            result_rx,
            bytes_rx,
            recv: self.clone(),
        }
    }

    pub fn deliver_pull_result(&self, job_id: &str, outcome: EvalCachePullOutcome) {
        let mut g = self.inner.lock();
        match g.pending.remove(job_id) {
            Some(Pending::Pull {
                result_tx,
                bytes_tx,
            }) => {
                if let EvalCachePullOutcome::Inline { total_bytes, .. } = &outcome {
                    g.pending.insert(
                        job_id.to_owned(),
                        Pending::PullStream {
                            buf: Vec::with_capacity(*total_bytes as usize),
                            bytes_tx,
                        },
                    );
                }
                drop(g);
                if result_tx.send(outcome).is_err() {
                    debug!(%job_id, "eval-cache pull waiter went away before delivery");
                }
            }
            _ => warn!(%job_id, "EvalCachePullResult with no pull waiter - discarding"),
        }
    }

    pub fn deliver_pull_chunk(&self, job_id: &str, data: &[u8], offset: u64, is_final: bool) {
        let mut g = self.inner.lock();
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

        buf.extend_from_slice(data);

        if is_final
            && let Some(Pending::PullStream { buf, bytes_tx }) = g.pending.remove(job_id)
            && bytes_tx.send(Ok(buf)).is_err()
        {
            debug!(%job_id, "eval-cache inline waiter went away before delivery");
        }
    }

    pub fn forget_job(&self, job_id: &str) {
        self.inner.lock().pending.remove(job_id);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

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
        r2.deliver_pull_chunk("j", b"abc", 0, false);
        r2.deliver_pull_chunk("j", b"def", 3, false);
        r2.deliver_pull_chunk("j", b"ghi", 6, true);
        assert_eq!(task.await.unwrap().unwrap(), b"abcdefghi");
    }

    #[tokio::test]
    async fn chunks_delivered_before_the_waiter_wakes_are_kept() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result(
            "j",
            EvalCachePullOutcome::Inline {
                total_bytes: 6,
                stream_token: "t".into(),
            },
        );
        r.deliver_pull_chunk("j", b"abc", 0, false);
        r.deliver_pull_chunk("j", b"def", 3, true);

        pull.await_outcome().await.unwrap();
        assert_eq!(pull.await_inline(6).await.unwrap(), b"abcdef");
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
        r2.deliver_pull_chunk("j", b"hello", 0, true);
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
        r2.deliver_pull_chunk("j", b"abc", 0, false);
        r2.deliver_pull_chunk("j", b"def", 99, true);
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
        r2.deliver_pull_chunk("j", b"short", 0, true);
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
    async fn forget_job_cancels_pull_waiter() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("doomed");
        r.forget_job("doomed");
        assert!(pull.await_outcome().await.is_err());
    }
}
