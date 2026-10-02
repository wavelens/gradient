/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::time::Duration;

use tracing::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    CleanDisconnect,
    Drained,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    Served,
    Refused,
    Drained,
}

impl From<RunOutcome> for SessionEnd {
    fn from(outcome: RunOutcome) -> Self {
        match outcome {
            RunOutcome::CleanDisconnect => SessionEnd::Served,
            RunOutcome::Refused => SessionEnd::Refused,
            RunOutcome::Drained => SessionEnd::Drained,
        }
    }
}

/// A handshake can succeed and still be refused or drained again. Resetting the backoff then would
/// hammer the server about once a second, and a session that served nothing is escalating instead.
pub fn backoff_after_session(
    previous: Duration,
    end: SessionEnd,
    initial: Duration,
    max: Duration,
) -> Duration {
    match end {
        SessionEnd::Served => initial,
        SessionEnd::Refused | SessionEnd::Drained => (previous * 2).min(max),
    }
}

pub async fn retry_reconnect<T, E, Sleep, SleepFut>(
    mut attempt: impl AsyncFnMut() -> Result<T, E>,
    mut sleep: Sleep,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> T
where
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
    E: std::fmt::Display,
{
    let mut backoff = initial_backoff;
    loop {
        sleep(backoff).await;
        match attempt().await {
            Ok(t) => return t,
            Err(e) => {
                error!(
                    error = %e,
                    delay_secs = backoff.as_secs(),
                    "reconnect failed; retrying"
                );
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn refused_sessions_escalate_the_backoff() {
        let initial = Duration::from_secs(1);
        let max = Duration::from_secs(60);

        let mut delay = initial;
        for expected in [2, 4, 8, 16, 32, 60, 60] {
            delay = backoff_after_session(delay, SessionEnd::Refused, initial, max);
            assert_eq!(delay, Duration::from_secs(expected));
        }
    }

    #[test]
    fn drained_sessions_escalate_the_backoff() {
        let initial = Duration::from_secs(1);
        let max = Duration::from_secs(60);

        let mut delay = initial;
        for expected in [2, 4, 8, 16, 32, 60, 60] {
            delay = backoff_after_session(delay, SessionEnd::Drained, initial, max);
            assert_eq!(delay, Duration::from_secs(expected));
        }
    }

    #[test]
    fn every_session_outcome_reconnects() {
        assert_eq!(
            SessionEnd::from(RunOutcome::CleanDisconnect),
            SessionEnd::Served
        );
        assert_eq!(SessionEnd::from(RunOutcome::Refused), SessionEnd::Refused);
        assert_eq!(SessionEnd::from(RunOutcome::Drained), SessionEnd::Drained);
    }

    #[test]
    fn served_sessions_reset_the_backoff() {
        let initial = Duration::from_secs(1);
        assert_eq!(
            backoff_after_session(
                Duration::from_secs(32),
                SessionEnd::Served,
                initial,
                Duration::from_secs(60)
            ),
            initial
        );
    }

    #[tokio::test]
    async fn keeps_retrying_after_failure() {
        let attempts = RefCell::new(0u32);
        let result: u32 = retry_reconnect(
            async || {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() < 4 {
                    Err::<u32, String>("transient".into())
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

    #[tokio::test]
    async fn backoff_caps_at_max() {
        let delays = RefCell::new(Vec::<Duration>::new());
        let attempts = RefCell::new(0u32);
        let _: u32 = retry_reconnect(
            async || {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() < 8 {
                    Err::<u32, String>("nope".into())
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
}
