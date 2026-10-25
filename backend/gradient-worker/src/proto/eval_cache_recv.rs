/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use gradient_wire::messages::{EvalCachePullOutcome, SegmentSource, TRANSFER_TIMEOUT};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

type SegmentBytes = Result<Vec<u8>, String>;

enum Pending {
    Pull {
        result_tx: oneshot::Sender<EvalCachePullOutcome>,
        segments_tx: mpsc::UnboundedSender<SegmentBytes>,
    },
    PullStream {
        remaining: usize,
        buf: Vec<u8>,
        broken: Option<String>,
        segments_tx: mpsc::UnboundedSender<SegmentBytes>,
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
    segments_rx: mpsc::UnboundedReceiver<SegmentBytes>,
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

    pub async fn await_inline_segment(&mut self) -> Result<Vec<u8>> {
        match tokio::time::timeout(TRANSFER_TIMEOUT, self.segments_rx.recv()).await {
            Ok(Some(Ok(bytes))) => Ok(bytes),
            Ok(Some(Err(reason))) => Err(anyhow::anyhow!(
                "eval-cache inline segment failed: {reason}"
            )),
            Ok(None) => Err(anyhow::anyhow!(
                "eval-cache inline stream closed (job_id={})",
                self.job_id
            )),
            Err(_) => {
                self.recv.forget_job(&self.job_id);
                Err(anyhow::anyhow!(
                    "eval-cache inline segment for job_id={} timed out after {}s",
                    self.job_id,
                    TRANSFER_TIMEOUT.as_secs(),
                ))
            }
        }
    }
}

fn inline_segments(outcome: &EvalCachePullOutcome) -> usize {
    match outcome {
        EvalCachePullOutcome::Segments { fetch, .. } => fetch
            .iter()
            .filter(|f| matches!(f.source, SegmentSource::Inline { .. }))
            .count(),
        _ => 0,
    }
}

impl EvalCacheReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_pull(&self, job_id: &str) -> PendingPull {
        let (result_tx, result_rx) = oneshot::channel();
        let (segments_tx, segments_rx) = mpsc::unbounded_channel();
        self.inner.lock().pending.insert(
            job_id.to_owned(),
            Pending::Pull {
                result_tx,
                segments_tx,
            },
        );
        PendingPull {
            job_id: job_id.to_owned(),
            result_rx,
            segments_rx,
            recv: self.clone(),
        }
    }

    pub fn deliver_pull_result(&self, job_id: &str, outcome: EvalCachePullOutcome) {
        let mut g = self.inner.lock();
        match g.pending.remove(job_id) {
            Some(Pending::Pull {
                result_tx,
                segments_tx,
            }) => {
                let remaining = inline_segments(&outcome);
                if remaining > 0 {
                    g.pending.insert(
                        job_id.to_owned(),
                        Pending::PullStream {
                            remaining,
                            buf: Vec::new(),
                            broken: None,
                            segments_tx,
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
        let Some(Pending::PullStream {
            remaining,
            buf,
            broken,
            segments_tx,
        }) = g.pending.get_mut(job_id)
        else {
            warn!(%job_id, "EvalCacheChunk with no inline pull stream - discarding");
            return;
        };

        if broken.is_none() && offset != buf.len() as u64 {
            *broken = Some(format!(
                "non-contiguous eval-cache chunk: offset {offset} != expected {}",
                buf.len()
            ));
        }

        if broken.is_none() {
            buf.extend_from_slice(data);
        }

        if !is_final {
            return;
        }

        let segment = match broken.take() {
            Some(reason) => Err(reason),
            None => Ok(std::mem::take(buf)),
        };
        buf.clear();
        if segments_tx.send(segment).is_err() {
            debug!(%job_id, "eval-cache inline waiter went away before delivery");
        }

        *remaining -= 1;
        if *remaining == 0 {
            g.pending.remove(job_id);
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
    use gradient_wire::messages::SegmentFetch;

    fn segments(inline: &[&str]) -> EvalCachePullOutcome {
        EvalCachePullOutcome::Segments {
            keys: inline.iter().map(|key| (*key).to_owned()).collect(),
            fetch: inline
                .iter()
                .map(|key| SegmentFetch {
                    key: (*key).to_owned(),
                    source: SegmentSource::Inline { size_bytes: 0 },
                })
                .collect(),
            stream_token: Some("ec-fp".into()),
        }
    }

    #[tokio::test]
    async fn inline_segments_arrive_back_to_back() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result("j", segments(&["a", "b"]));
        pull.await_outcome().await.unwrap();

        r.deliver_pull_chunk("j", b"abc", 0, false);
        r.deliver_pull_chunk("j", b"def", 3, true);
        r.deliver_pull_chunk("j", b"ghi", 0, true);

        assert_eq!(pull.await_inline_segment().await.unwrap(), b"abcdef");
        assert_eq!(pull.await_inline_segment().await.unwrap(), b"ghi");
        assert!(pull.await_inline_segment().await.is_err());
    }

    #[tokio::test]
    async fn chunks_delivered_before_the_waiter_wakes_are_kept() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result("j", segments(&["a"]));
        r.deliver_pull_chunk("j", b"abc", 0, false);
        r.deliver_pull_chunk("j", b"def", 3, true);

        pull.await_outcome().await.unwrap();
        assert_eq!(pull.await_inline_segment().await.unwrap(), b"abcdef");
    }

    #[tokio::test]
    async fn a_non_contiguous_chunk_fails_only_its_segment() {
        let r = EvalCacheReceiver::new();
        let mut pull = r.register_pull("j");
        r.deliver_pull_result("j", segments(&["a", "b"]));
        pull.await_outcome().await.unwrap();

        r.deliver_pull_chunk("j", b"abc", 0, false);
        r.deliver_pull_chunk("j", b"def", 99, false);
        r.deliver_pull_chunk("j", b"", 6, true);
        r.deliver_pull_chunk("j", b"next", 0, true);

        let err = pull.await_inline_segment().await.unwrap_err().to_string();
        assert!(err.contains("non-contiguous"), "got: {err}");
        assert_eq!(pull.await_inline_segment().await.unwrap(), b"next");
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
