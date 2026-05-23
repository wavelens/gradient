/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Centralized graceful-shutdown primitive.
//!
//! `Shutdown` bundles a [`CancellationToken`] (the signal) with a
//! [`TaskTracker`] (the registry). Long-lived background tasks are spawned
//! via `spawn` so the process can drain them on SIGTERM/SIGINT instead of
//! abandoning in-flight cleanups, metric writes, and webhook deliveries.
//!
//! # Rules
//!
//! - Anything outliving a single request goes through `Shutdown::spawn`,
//!   never bare `tokio::spawn`.
//! - Loops that sleep (`interval.tick`, `sleep`) must `select!` on
//!   `cancelled()` so SIGTERM doesn't have to wait a full poll cycle.
//! - Per-connection / per-job tasks derive a child token via
//!   [`Shutdown::child_token`] so cancelling the parent cancels them
//!   transitively.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, info, warn};

#[derive(Clone, Debug)]
pub struct Shutdown {
    token: CancellationToken,
    tracker: TaskTracker,
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
        }
    }

    /// Cancellation token. `cancelled().await` resolves once shutdown is
    /// requested. Cheap to clone.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Future that resolves when shutdown has been requested.
    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// A child token that is cancelled when the parent token is cancelled,
    /// or independently. Used for per-connection / per-job scopes that
    /// should be cancellable on their own without affecting siblings.
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// Register a background task with the tracker. Replaces bare
    /// `tokio::spawn` for anything outliving a single request.
    ///
    /// The future is instrumented with the current `tracing` span, so cleanup
    /// work spawned from inside an HTTP handler keeps the request span (and
    /// therefore the request-id) on every log line. Outside of a request
    /// `Span::current()` is the root no-op span - instrumenting is then
    /// effectively free.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(future.in_current_span())
    }

    /// Trigger shutdown. Idempotent.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Cancel and drain all tracked tasks, bounded by `timeout`. Returns
    /// `true` if all tasks completed before the deadline.
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

    /// Number of currently-tracked tasks. Useful for tests / introspection.
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
            // Ignores the cancel signal - simulates a misbehaving task.
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
