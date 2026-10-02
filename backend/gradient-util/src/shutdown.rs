/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Loops must go through `Shutdown::supervise`. Tasks outliving one request must go through
//! `Shutdown::spawn`, never bare `tokio::spawn`. Sleeping loops must `select!` on `cancelled()` to
//! avoid delaying SIGTERM by a full poll cycle.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, error, info, warn};

use crate::supervision::{ChildSpec, Supervisor, SupervisorHealth};

pub use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Shutdown {
    token: CancellationToken,
    tracker: TaskTracker,
    tree: Arc<OnceCell<Supervisor>>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            tracker: TaskTracker::new(),
            tree: Arc::new(OnceCell::new()),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(future.in_current_span())
    }

    pub async fn supervisor(&self) -> Result<&Supervisor, String> {
        self.tree
            .get_or_try_init(|| async {
                Supervisor::start(self.token.clone(), self.tracker.clone())
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
    }

    pub fn tree(&self) -> Option<&Supervisor> {
        self.tree.get()
    }

    pub fn supervision_health(&self) -> Option<Arc<SupervisorHealth>> {
        self.tree.get().map(Supervisor::health)
    }

    pub async fn supervise_now(&self, spec: ChildSpec) -> Result<(), String> {
        self.supervisor().await?.add(spec).await
    }

    pub fn supervise(&self, spec: ChildSpec) {
        let this = self.clone();
        let name = spec.name();
        self.spawn(async move {
            if let Err(error) = this.supervise_now(spec).await {
                error!(loop_name = name, %error, "failed to start supervised loop");
            }
        });
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub async fn cancel_and_drain(&self, timeout: Duration) -> bool {
        self.cancel();
        self.tracker.close();
        match tokio::time::timeout(timeout, self.tracker.wait()).await {
            Ok(()) => {
                info!("background tasks drained cleanly");
                true
            }
            Err(_) => {
                warn!(
                    pending = self.tracker.len(),
                    "shutdown drain timed out - some background tasks were abandoned"
                );
                false
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.tracker.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn cancel_interrupts_select_loop() {
        let s = Shutdown::new();
        let token = s.token();
        let done = Arc::new(AtomicBool::new(false));
        let done2 = Arc::clone(&done);

        let handle = s.spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
            }
            done2.store(true, Ordering::SeqCst);
        });

        s.cancel_and_drain(Duration::from_secs(1)).await;
        assert!(done.load(Ordering::SeqCst));
        assert!(handle.is_finished());
    }

    #[tokio::test]
    async fn drain_waits_for_in_flight_work() {
        let s = Shutdown::new();
        let done = Arc::new(AtomicBool::new(false));
        let done2 = Arc::clone(&done);
        s.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            done2.store(true, Ordering::SeqCst);
        });
        let drained = s.cancel_and_drain(Duration::from_secs(1)).await;
        assert!(drained);
        assert!(done.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drain_timeout_returns_false() {
        let s = Shutdown::new();
        s.spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let drained = s.cancel_and_drain(Duration::from_millis(50)).await;
        assert!(!drained);
    }

    #[tokio::test]
    async fn child_token_cascades_from_parent() {
        let s = Shutdown::new();
        let child = s.child_token();
        assert!(!child.is_cancelled());
        s.cancel();
        assert!(child.is_cancelled());
    }
}
