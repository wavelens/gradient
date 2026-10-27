/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use futures::{Stream, StreamExt};
use tokio::time::Instant;

use crate::executor::AbortSignal;

#[derive(Debug, PartialEq, Eq)]
pub enum Watched<T> {
    Item(T),
    End,
    Aborted,
    Silent(Duration),
    TotalExceeded(Duration),
}

pub struct Watchdog {
    started: Instant,
    total: Option<Duration>,
    silent: Option<Duration>,
    last_output: Instant,
}

impl Watchdog {
    pub fn new(started: Instant, total: Option<Duration>, silent: Option<Duration>) -> Self {
        Self {
            started,
            total,
            silent,
            last_output: started,
        }
    }

    pub fn saw_output(&mut self) {
        self.last_output = Instant::now();
    }

    pub async fn next<S: Stream + Unpin>(
        &mut self,
        stream: &mut S,
        abort: &mut AbortSignal,
    ) -> Watched<S::Item> {
        if abort.is_aborted() {
            return Watched::Aborted;
        }
        tokio::select! {
            biased;
            () = abort.aborted() => Watched::Aborted,
            total = expires(self.started, self.total) => Watched::TotalExceeded(total),
            silent = expires(self.last_output, self.silent) => Watched::Silent(silent),
            item = stream.next() => item.map_or(Watched::End, Watched::Item),
        }
    }
}

async fn expires(from: Instant, limit: Option<Duration>) -> Duration {
    match limit {
        Some(limit) => {
            tokio::time::sleep_until(from + limit).await;
            limit
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn first_event(
        watchdog: &mut Watchdog,
        stream: &mut (impl futures::Stream<Item = u32> + Unpin),
    ) -> (Watched<u32>, Duration) {
        let (_tx, mut abort) = AbortSignal::channel();
        let started = Instant::now();
        let event = watchdog.next(stream, &mut abort).await;
        (event, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn output_right_before_max_silent_pushes_the_deadline() {
        let start = Instant::now();
        let mut watchdog = Watchdog::new(start, None, Some(Duration::from_secs(10)));
        let mut stream = futures::stream::once(async {
            tokio::time::sleep(Duration::from_secs(9)).await;
            7
        })
        .chain(futures::stream::pending())
        .boxed();

        let (event, _) = first_event(&mut watchdog, &mut stream).await;
        assert_eq!(event, Watched::Item(7));
        watchdog.saw_output();

        let (event, _) = first_event(&mut watchdog, &mut stream).await;
        assert_eq!(event, Watched::Silent(Duration::from_secs(10)));
        assert_eq!(start.elapsed(), Duration::from_secs(19));
    }

    #[tokio::test(start_paused = true)]
    async fn the_total_timeout_fires_while_output_flows() {
        let start = Instant::now();
        let mut watchdog = Watchdog::new(
            start,
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(10)),
        );
        let mut stream = futures::stream::unfold(0u32, |n| async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Some((n, n + 1))
        })
        .boxed();

        let outcome = loop {
            match first_event(&mut watchdog, &mut stream).await.0 {
                Watched::Item(_) => watchdog.saw_output(),
                other => break other,
            }
        };
        assert_eq!(outcome, Watched::TotalExceeded(Duration::from_secs(30)));
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_program_times_out() {
        let mut watchdog = Watchdog::new(Instant::now(), None, Some(Duration::from_secs(10)));
        let mut stream = futures::stream::pending::<u32>();

        let (event, waited) = first_event(&mut watchdog, &mut stream).await;
        assert_eq!(event, Watched::Silent(Duration::from_secs(10)));
        assert_eq!(waited, Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn an_abort_wins_over_a_ready_item() {
        let mut watchdog = Watchdog::new(Instant::now(), None, None);
        let mut stream = futures::stream::iter([1u32]);
        let (tx, mut abort) = AbortSignal::channel();
        tx.send(true).unwrap();

        assert_eq!(
            watchdog.next(&mut stream, &mut abort).await,
            Watched::Aborted
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_abort_ends_the_wait_on_a_stalled_stream() {
        let mut watchdog = Watchdog::new(Instant::now(), None, None);
        let mut stream = futures::stream::pending::<u32>();
        let (tx, mut abort) = AbortSignal::channel();
        let mut next = std::pin::pin!(watchdog.next(&mut stream, &mut abort));

        assert!(futures::poll!(next.as_mut()).is_pending());
        tx.send(true).unwrap();
        assert_eq!(next.await, Watched::Aborted);
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_of_the_stream_is_reported() {
        let mut watchdog = Watchdog::new(Instant::now(), None, Some(Duration::from_secs(1)));
        let mut stream = futures::stream::empty::<u32>();

        assert_eq!(
            first_event(&mut watchdog, &mut stream).await.0,
            Watched::End
        );
    }
}
