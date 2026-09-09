/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Two-stage local stop for the worker process: the first signal drains (no
//! new work, in-flight jobs finish and report), the second signal or the
//! expired drain budget aborts them. A server draining its own sessions only
//! ends that session; the run loop reconnects until it is back (#626).

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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

    /// First stage: stop taking work, keep what is already running.
    pub fn request_drain(&self) {
        self.drain.cancel();
    }

    /// Second stage: abandon in-flight work. Implies the drain, so a straight
    /// abort never looks like a live worker.
    pub fn request_abort(&self) {
        self.drain.cancel();
        self.abort.cancel();
    }

    /// The process is on its way out, so the run loop must not reconnect.
    pub fn is_stopping(&self) -> bool {
        self.drain.is_cancelled()
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

/// Drive the stop sequence from a source of stop signals: the first drains,
/// the second aborts, and so does the drain `budget` running out (`None`
/// waits for in-flight jobs however long they take).
pub async fn stop_sequence<S, F>(shutdown: &Shutdown, budget: Option<Duration>, mut signal: S)
where
    S: FnMut() -> F,
    F: Future<Output = ()>,
{
    signal().await;
    info!("stop requested; draining: no new jobs, finishing the in-flight ones");
    shutdown.request_drain();

    tokio::select! {
        () = signal() => warn!("second stop signal; abandoning in-flight jobs"),
        () = budget_elapsed(budget) => warn!(
            budget_secs = budget.map_or(0, |b| b.as_secs()),
            "drain budget expired; abandoning in-flight jobs"
        ),
    }
    shutdown.request_abort();
}

async fn budget_elapsed(budget: Option<Duration>) {
    match budget {
        Some(budget) => tokio::time::sleep(budget).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use std::cell::Cell;
    use std::pin::Pin;

    fn aborting(shutdown: &Shutdown) -> bool {
        shutdown.abort_requested().now_or_never().is_some()
    }

    /// `n` stop signals arrive at once, then none ever again.
    fn signals(n: u32) -> impl FnMut() -> Pin<Box<dyn Future<Output = ()>>> {
        let left = Cell::new(n);
        move || -> Pin<Box<dyn Future<Output = ()>>> {
            if left.get() == 0 {
                return Box::pin(std::future::pending());
            }
            left.set(left.get() - 1);
            Box::pin(std::future::ready(()))
        }
    }

    /// Nothing is requested until a signal arrives: a fresh handle must not
    /// read as stopping, or the worker would exit before it ever connected.
    #[test]
    fn a_fresh_shutdown_is_idle() {
        let shutdown = Shutdown::new();

        assert!(!shutdown.is_stopping());
        assert!(!aborting(&shutdown));
    }

    /// The first signal only drains: in-flight jobs keep running while the run
    /// loop already knows not to reconnect.
    #[tokio::test(start_paused = true)]
    async fn the_first_signal_drains_without_aborting() {
        let shutdown = Shutdown::new();

        let ended = tokio::time::timeout(
            Duration::from_secs(1),
            stop_sequence(&shutdown, None, signals(1)),
        )
        .await;

        assert!(
            ended.is_err(),
            "one signal without a budget never ends the sequence"
        );
        assert!(shutdown.is_stopping());
        assert!(!aborting(&shutdown));
    }

    /// An abort implies the drain, so a worker killed outright is never
    /// mistaken for one that should reconnect.
    #[test]
    fn an_abort_implies_the_drain() {
        let shutdown = Shutdown::new();
        shutdown.request_abort();

        assert!(shutdown.is_stopping());
        assert!(aborting(&shutdown));
    }

    /// The drain budget is the only thing standing between a stuck build and a
    /// `systemctl stop` that never returns.
    #[tokio::test(start_paused = true)]
    async fn the_drain_budget_escalates_to_an_abort() {
        let shutdown = Shutdown::new();

        tokio::time::timeout(
            Duration::from_secs(601),
            stop_sequence(&shutdown, Some(Duration::from_secs(600)), signals(1)),
        )
        .await
        .expect("the budget ends the sequence");

        assert!(aborting(&shutdown));
    }

    /// A budget of `None` waits for in-flight jobs however long they take;
    /// only a second signal cuts that short.
    #[tokio::test(start_paused = true)]
    async fn an_unbounded_drain_never_escalates_on_its_own() {
        let shutdown = Shutdown::new();

        let ended = tokio::time::timeout(
            Duration::from_secs(86_400),
            stop_sequence(&shutdown, None, signals(1)),
        )
        .await;

        assert!(ended.is_err());
        assert!(!aborting(&shutdown));
    }

    /// The second signal aborts at once, budget or not.
    #[tokio::test]
    async fn a_second_signal_aborts_at_once() {
        let shutdown = Shutdown::new();

        stop_sequence(&shutdown, None, signals(2)).await;

        assert!(aborting(&shutdown));
    }
}
