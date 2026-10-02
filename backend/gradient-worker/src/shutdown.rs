/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A server draining its own sessions is ending only that session. The run loop is reconnecting
//! until the server is back (#626).

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    drain: CancellationToken,
    abort: CancellationToken,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_drain(&self) {
        self.drain.cancel();
    }

    /// Abort is implying the drain. A straight abort must never look like a live worker.
    pub fn request_abort(&self) {
        self.drain.cancel();
        self.abort.cancel();
    }

    pub fn is_stopping(&self) -> bool {
        self.drain.is_cancelled()
    }

    pub fn drain_requested(&self) -> impl Future<Output = ()> + '_ {
        self.drain.cancelled()
    }

    pub fn abort_requested(&self) -> impl Future<Output = ()> + '_ {
        self.abort.cancelled()
    }
}

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

    #[test]
    fn a_fresh_shutdown_is_idle() {
        let shutdown = Shutdown::new();

        assert!(!shutdown.is_stopping());
        assert!(!aborting(&shutdown));
    }

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

    #[test]
    fn an_abort_implies_the_drain() {
        let shutdown = Shutdown::new();
        shutdown.request_abort();

        assert!(shutdown.is_stopping());
        assert!(aborting(&shutdown));
    }

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

    #[tokio::test]
    async fn a_second_signal_aborts_at_once() {
        let shutdown = Shutdown::new();

        stop_sequence(&shutdown, None, signals(2)).await;

        assert!(aborting(&shutdown));
    }
}
