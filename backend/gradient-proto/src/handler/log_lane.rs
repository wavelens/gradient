/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::sync::Arc;

use gradient_db::WorkerDb;
use gradient_storage::LogStorage;
use gradient_types::ids::{DerivationBuildId, ExecJobAttemptId};
use gradient_util::shutdown::Shutdown;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

const LOG_QUEUE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogTarget {
    Build(DerivationBuildId),
    ExecAttempt(ExecJobAttemptId),
}

enum Entry {
    Chunk { target: LogTarget, data: Vec<u8> },
    Flush(oneshot::Sender<()>),
    After(oneshot::Receiver<()>),
}

pub(super) struct LogLane {
    tx: mpsc::Sender<Entry>,
}

impl LogLane {
    pub(super) fn spawn<F, Fut>(shutdown: &Shutdown, append: F) -> Self
    where
        F: Fn(LogTarget, Vec<u8>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (tx, mut rx) = mpsc::channel(LOG_QUEUE);
        shutdown.spawn(async move {
            while let Some(entry) = rx.recv().await {
                match entry {
                    Entry::Chunk { target, data } => append(target, data).await,
                    Entry::Flush(done) => {
                        let _ = done.send(());
                    }
                    Entry::After(applied) => {
                        let _ = applied.await;
                    }
                }
            }
        });
        Self { tx }
    }

    pub(super) fn to_storage(
        shutdown: &Shutdown,
        db: WorkerDb,
        log_storage: Arc<dyn LogStorage>,
    ) -> Self {
        Self::spawn(shutdown, move |target, data| {
            let db = db.clone();
            let log_storage = Arc::clone(&log_storage);
            async move { append_to_storage(&db, log_storage.as_ref(), target, &data).await }
        })
    }

    pub(super) async fn append(&self, target: LogTarget, data: Vec<u8>) {
        let _ = self.tx.send(Entry::Chunk { target, data }).await;
    }

    pub(super) async fn after(&self, applied: oneshot::Receiver<()>) {
        let _ = self.tx.send(Entry::After(applied)).await;
    }

    /// A job's last lines must land before its completion is closing the log.
    pub(super) async fn flushed(&self) -> oneshot::Receiver<()> {
        let (done, flushed) = oneshot::channel();
        let _ = self.tx.send(Entry::Flush(done)).await;
        flushed
    }
}

async fn append_to_storage(
    db: &WorkerDb,
    log_storage: &dyn LogStorage,
    target: LogTarget,
    data: &[u8],
) {
    let attempt = match target {
        LogTarget::ExecAttempt(attempt) => gradient_db::exec_jobs::log_key(attempt),
        LogTarget::Build(build) => {
            match gradient_db::scheduling::build_attempt::running_attempt_id(db, build)
                .await
                .unwrap_or(None)
            {
                Some(attempt) => attempt,
                None => {
                    debug!(%build, bytes = data.len(), "log chunk dropped: no running attempt for the shared build");
                    return;
                }
            }
        }
    };

    if let Err(e) = log_storage
        .append(attempt, &String::from_utf8_lossy(data))
        .await
    {
        debug!(?target, error = %e, "log append failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_flush_returns_once_every_earlier_chunk_is_written() {
        let shutdown = Shutdown::new();
        let written = Arc::new(gradient_util::sync::Mutex::new(Vec::new()));
        let lane = LogLane::spawn(&shutdown, {
            let written = Arc::clone(&written);
            move |_, data| {
                let written = Arc::clone(&written);
                async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    written.lock().push(data);
                }
            }
        });
        let build = LogTarget::Build(DerivationBuildId::now_v7());

        lane.append(build, b"one".to_vec()).await;
        lane.append(build, b"two".to_vec()).await;
        let _ = lane.flushed().await.await;

        assert_eq!(*written.lock(), vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[tokio::test]
    async fn an_exec_chunk_reaches_the_attempt_log() {
        let shutdown = Shutdown::new();
        let written = Arc::new(gradient_util::sync::Mutex::new(Vec::new()));
        let lane = LogLane::spawn(&shutdown, {
            let written = Arc::clone(&written);
            move |target, data| {
                let written = Arc::clone(&written);
                async move { written.lock().push((target, data)) }
            }
        });
        let attempt = ExecJobAttemptId::now_v7();

        lane.append(LogTarget::ExecAttempt(attempt), b"hi".to_vec())
            .await;
        let _ = lane.flushed().await.await;

        assert_eq!(
            *written.lock(),
            vec![(LogTarget::ExecAttempt(attempt), b"hi".to_vec())]
        );
    }

    #[tokio::test]
    async fn chunks_after_a_build_start_wait_until_the_start_is_applied() {
        let shutdown = Shutdown::new();
        let written = Arc::new(gradient_util::sync::Mutex::new(Vec::new()));
        let lane = LogLane::spawn(&shutdown, {
            let written = Arc::clone(&written);
            move |_, data| {
                let written = Arc::clone(&written);
                async move { written.lock().push(data) }
            }
        });
        let build = LogTarget::Build(DerivationBuildId::now_v7());
        let (start_applied, applied) = oneshot::channel();

        lane.append(build, b"before".to_vec()).await;
        lane.after(applied).await;
        lane.append(build, b"after".to_vec()).await;
        let flushed = lane.flushed().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*written.lock(), vec![b"before".to_vec()]);

        start_applied.send(()).unwrap();
        let _ = flushed.await;
        assert_eq!(*written.lock(), vec![b"before".to_vec(), b"after".to_vec()]);
    }
}
