/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Reconnect-with-backoff helper.
//!
//! Drives `Worker::reconnect` (and any Disconnected → Connected transition that
//! returns its state on failure) until it succeeds, doubling the delay between
//! attempts up to `max_backoff`. Lives in its own module so the loop can be
//! unit-tested without standing up a real `Worker`.

use std::future::Future;
use std::time::Duration;

use tracing::error;

/// Retry `attempt` indefinitely with exponential backoff.
///
/// `attempt` consumes the current state and returns either the next state on
/// success, or `(error, state)` on failure so the next iteration can keep
/// driving the same state. `sleep` is parameterised so tests can substitute a
/// no-op timer.
pub async fn retry_reconnect<S, T, E, F, Fut, Sleep, SleepFut>(
    initial_state: S,
    mut attempt: F,
    mut sleep: Sleep,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> T
where
    F: FnMut(S) -> Fut,
    Fut: Future<Output = Result<T, (E, S)>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
    E: std::fmt::Display,
{
    let mut state = initial_state;
    let mut backoff = initial_backoff;
    loop {
        sleep(backoff).await;
        match attempt(state).await {
            Ok(t) => return t,
            Err((e, s)) => {
                error!(
                    error = %e,
                    delay_secs = backoff.as_secs(),
                    "reconnect failed; retrying"
                );
                state = s;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Regression for #99: the loop must keep retrying after a single failure
    /// instead of breaking out and shutting the worker down.
    #[tokio::test]
    async fn keeps_retrying_after_failure() {
        let attempts = RefCell::new(0u32);
        let result: u32 = retry_reconnect(
            (),
            |()| async {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() < 4 {
                    Err::<u32, (String, ())>(("transient".into(), ()))
                } else {
                    Ok(42)
                }
            },
            |_d| async {},
            Duration::from_millis(1),
            Duration::from_millis(8),
        )
        .await;

        assert_eq!(result, 42);
        assert_eq!(*attempts.borrow(), 4);
    }

    /// Backoff must double until it hits `max_backoff` and then plateau —
    /// guards against an off-by-one where an unbounded multiplication could
    /// overflow `Duration` after enough retries.
    #[tokio::test]
    async fn backoff_caps_at_max() {
        let delays = RefCell::new(Vec::<Duration>::new());
        let attempts = RefCell::new(0u32);
        let _: u32 = retry_reconnect(
            (),
            |()| async {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() < 8 {
                    Err::<u32, (String, ())>(("nope".into(), ()))
                } else {
                    Ok(0)
                }
            },
            |d| {
                delays.borrow_mut().push(d);
                async {}
            },
            Duration::from_secs(1),
            Duration::from_secs(8),
        )
        .await;

        let observed = delays.borrow().clone();
        assert_eq!(observed[0], Duration::from_secs(1));
        assert_eq!(observed[1], Duration::from_secs(2));
        assert_eq!(observed[2], Duration::from_secs(4));
        assert_eq!(observed[3], Duration::from_secs(8));
        assert_eq!(observed[4], Duration::from_secs(8));
        assert_eq!(observed[5], Duration::from_secs(8));
    }

    /// Verifies the typestate-preservation contract: each retry receives the
    /// same state value the previous attempt returned, so cached resources
    /// (executor, scorer, credentials in the real `Worker<Disconnected>`)
    /// are not lost across retries.
    #[tokio::test]
    async fn state_threads_through_retries() {
        let attempts = RefCell::new(0u32);
        let result: String = retry_reconnect(
            String::from("session-A"),
            |s| {
                *attempts.borrow_mut() += 1;
                let attempt = *attempts.borrow();
                async move {
                    if attempt < 3 {
                        Err::<String, (String, String)>(("retry".into(), s))
                    } else {
                        Ok(s)
                    }
                }
            },
            |_d| async {},
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .await;

        assert_eq!(result, "session-A");
    }
}
