/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Two-stage local stop for the worker process.
//!
//! The first signal *drains*: every session tells its server it wants no more
//! work (`ClientMessage::Draining`), finishes and reports the jobs it already
//! has, then the process exits. The drain arms a budget; when it expires - or
//! on a second signal - the stop *aborts*, in-flight jobs are killed and the
//! server re-queues them.
//!
//! Only these local signals end the worker. A server draining its own sessions
//! (deploy, maintenance) just ends that session; the run loop reconnects until
//! the server is back, and any other server stays served (#626).

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Handle on the worker's stop sequence, cloned into every loop that observes
/// it. Both stages are one-way, so the sequence can only ever move forward.
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    drain: CancellationToken,
    abort: CancellationToken,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// First signal: stop taking work, keep what is already running, and arm
    /// the budget after which the rest is abandoned. `budget` of `None` waits
    /// for in-flight jobs however long they take.
    pub fn request_drain(&self, budget: Option<Duration>) {
        if self.drain.is_cancelled() {
            return;
        }

        self.drain.cancel();
        let Some(budget) = budget else {
            return;
        };

        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(budget).await;
            if !this.is_aborting() {
                warn!(
                    budget_secs = budget.as_secs(),
                    "drain budget expired; abandoning in-flight jobs"
                );
                this.request_abort();
            }
        });
    }

    /// Second signal, or an expired drain budget: abandon in-flight work.
    /// Implies the drain, so a straight abort never looks like a live worker.
    pub fn request_abort(&self) {
        self.drain.cancel();
        self.abort.cancel();
    }

    /// The process is on its way out, so the run loop must not reconnect.
    pub fn is_stopping(&self) -> bool {
        self.drain.is_cancelled()
    }

    /// In-flight work is to be abandoned rather than waited for.
    pub fn is_aborting(&self) -> bool {
        self.abort.is_cancelled()
    }

    /// Resolves once a drain (or an abort) has been requested.
    pub fn drain_requested(&self) -> impl Future<Output = ()> + '_ {
        self.drain.cancelled()
    }

    /// Resolves once in-flight work is to be abandoned.
    pub fn abort_requested(&self) -> impl Future<Output = ()> + '_ {
        self.abort.cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing is requested until a signal arrives: a fresh handle must not
    /// read as stopping, or the worker would exit before it ever connected.
    #[test]
    fn a_fresh_shutdown_is_idle() {
        let shutdown = Shutdown::new();

        assert!(!shutdown.is_stopping());
        assert!(!shutdown.is_aborting());
    }

    /// The first signal only drains: in-flight jobs keep running while the run
    /// loop already knows not to reconnect.
    #[tokio::test]
    async fn a_drain_is_not_an_abort() {
        let shutdown = Shutdown::new();
        shutdown.request_drain(None);

        assert!(shutdown.is_stopping());
        assert!(!shutdown.is_aborting());
    }

    /// An abort implies the drain, so a worker killed outright is never
    /// mistaken for one that should reconnect.
    #[test]
    fn an_abort_implies_the_drain() {
        let shutdown = Shutdown::new();
        shutdown.request_abort();

        assert!(shutdown.is_stopping());
        assert!(shutdown.is_aborting());
    }

    /// The drain budget is the only thing standing between a stuck build and a
    /// `systemctl stop` that never returns.
    #[tokio::test(start_paused = true)]
    async fn the_drain_budget_escalates_to_an_abort() {
        let shutdown = Shutdown::new();
        shutdown.request_drain(Some(Duration::from_secs(600)));
        assert!(!shutdown.is_aborting());

        tokio::time::sleep(Duration::from_secs(601)).await;

        assert!(shutdown.is_aborting());
    }

    /// A budget of `None` waits for in-flight jobs however long they take;
    /// only a second signal cuts that short.
    #[tokio::test(start_paused = true)]
    async fn an_unbounded_drain_never_escalates_on_its_own() {
        let shutdown = Shutdown::new();
        shutdown.request_drain(None);

        tokio::time::sleep(Duration::from_secs(86_400)).await;

        assert!(!shutdown.is_aborting());
    }
}
