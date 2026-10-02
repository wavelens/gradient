/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_types::ids::DerivationBuildId;
use gradient_util::shutdown::Shutdown;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

const LOG_QUEUE: usize = 1024;

enum Entry {
    Chunk {
        build: DerivationBuildId,
        data: Vec<u8>,
    },
    Flush(oneshot::Sender<()>),
}

pub(super) struct LogLane {
    tx: mpsc::Sender<Entry>,
}

impl LogLane {
    pub(super) fn spawn<F, Fut>(shutdown: &Shutdown, append: F) -> Self
    where
        F: Fn(DerivationBuildId, Vec<u8>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (tx, mut rx) = mpsc::channel(LOG_QUEUE);
        shutdown.spawn(async move {
            while let Some(entry) = rx.recv().await {
                match entry {
                    Entry::Chunk { build, data } => append(build, data).await,
                    Entry::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self { tx }
    }

    pub(super) fn to_storage(shutdown: &Shutdown, state: Arc<ServerState>) -> Self {
        Self::spawn(shutdown, move |build, data| {
            let state = Arc::clone(&state);
            async move { append_to_storage(&state, build, &data).await }
        })
    }

    pub(super) async fn append(&self, build: DerivationBuildId, data: Vec<u8>) {
        let _ = self.tx.send(Entry::Chunk { build, data }).await;
    }

    /// A job's last lines must land before its completion is closing the log.
    pub(super) async fn flush(&self) {
        let (done, flushed) = oneshot::channel();
        if self.tx.send(Entry::Flush(done)).await.is_ok() {
            let _ = flushed.await;
        }
    }
}

async fn append_to_storage(state: &ServerState, build: DerivationBuildId, data: &[u8]) {
    let Some(attempt) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&state.worker_db, build)
            .await
            .unwrap_or(None)
    else {
        debug!(%build, bytes = data.len(), "log chunk dropped: no open attempt for the shared build");
        return;
    };

    if let Err(e) = state
        .log_storage
        .append(attempt, &String::from_utf8_lossy(data))
        .await
    {
        debug!(%build, error = %e, "log append failed");
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
        let build = DerivationBuildId::now_v7();

        lane.append(build, b"one".to_vec()).await;
        lane.append(build, b"two".to_vec()).await;
        lane.flush().await;

        assert_eq!(*written.lock(), vec![b"one".to_vec(), b"two".to_vec()]);
    }
}
