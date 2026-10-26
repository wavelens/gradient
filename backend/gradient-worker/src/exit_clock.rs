/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gradient_util::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExitLimits {
    pub idle_timeout: Option<Duration>,
    pub max_lifetime: Option<Duration>,
}

impl ExitLimits {
    pub fn is_set(&self) -> bool {
        self.idle_timeout.is_some() || self.max_lifetime.is_some()
    }
}

// One clock per process: every session reports its busy slots here, so an exit limit judges the
// whole worker and never one session alone.
pub struct ExitClock {
    started: Instant,
    state: Mutex<ClockState>,
}

struct ClockState {
    busy: HashMap<u64, usize>,
    idle_since: Instant,
    exiting: bool,
}

static CLOCK: LazyLock<ExitClock> = LazyLock::new(|| ExitClock::new(Instant::now()));
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

pub fn clock() -> &'static ExitClock {
    &CLOCK
}

pub fn new_session() -> u64 {
    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
}

impl ExitClock {
    fn new(started: Instant) -> Self {
        Self {
            started,
            state: Mutex::new(ClockState {
                busy: HashMap::new(),
                idle_since: started,
                exiting: false,
            }),
        }
    }

    pub fn report(&self, session: u64, busy: usize, now: Instant) {
        let mut state = self.state.lock();
        let was_idle = state.busy.is_empty();
        if busy == 0 {
            state.busy.remove(&session);
        } else {
            state.busy.insert(session, busy);
        }
        let is_idle = state.busy.is_empty();
        if is_idle && !was_idle {
            state.idle_since = now;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.state.lock().busy.is_empty()
    }

    pub fn exit_due(&self, limits: &ExitLimits, now: Instant) -> bool {
        let mut state = self.state.lock();
        let idle_since = state.busy.is_empty().then_some(state.idle_since);
        state.exiting = state.exiting || exit_due(idle_since, self.started, now, limits);
        state.exiting
    }

    pub fn deadline(&self, limits: &ExitLimits) -> Option<Instant> {
        let state = self.state.lock();
        let lifetime = limits.max_lifetime.map(|l| self.started + l);
        let idle = limits
            .idle_timeout
            .filter(|_| state.busy.is_empty())
            .map(|t| state.idle_since + t);
        lifetime.into_iter().chain(idle).min()
    }

    pub fn lifetime_left(&self, limits: &ExitLimits, now: Instant) -> Option<Duration> {
        limits
            .max_lifetime
            .map(|lifetime| (self.started + lifetime).saturating_duration_since(now))
    }
}

fn exit_due(
    idle_since: Option<Instant>,
    started: Instant,
    now: Instant,
    limits: &ExitLimits,
) -> bool {
    let elapsed = |since: Instant| now.saturating_duration_since(since);
    let idle_over = limits
        .idle_timeout
        .zip(idle_since)
        .is_some_and(|(timeout, since)| elapsed(since) >= timeout);
    let lifetime_over = limits
        .max_lifetime
        .is_some_and(|lifetime| elapsed(started) >= lifetime);
    idle_over || lifetime_over
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(idle: Option<u64>, lifetime: Option<u64>) -> ExitLimits {
        ExitLimits {
            idle_timeout: idle.map(Duration::from_secs),
            max_lifetime: lifetime.map(Duration::from_secs),
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn idle_past_timeout_exits() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        assert!(clock.exit_due(&limits(Some(60), None), started + secs(120)));
    }

    #[test]
    fn busy_never_exits_on_idle() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        clock.report(1, 1, started);
        assert!(!clock.exit_due(&limits(Some(60), None), started + secs(3600)));
    }

    #[test]
    fn a_held_cluster_slot_counts_as_busy() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        let held_slots = 1;
        clock.report(1, held_slots, started + secs(10));
        assert!(!clock.exit_due(&limits(Some(60), None), started + secs(600)));
    }

    #[test]
    fn job_done_resets_idle() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        clock.report(1, 1, started + secs(10));
        clock.report(1, 0, started + secs(100));
        let idle = limits(Some(60), None);
        assert!(!clock.exit_due(&idle, started + secs(130)));
        assert!(clock.exit_due(&idle, started + secs(160)));
    }

    #[test]
    fn one_idle_session_does_not_end_a_busy_one() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        clock.report(1, 0, started);
        clock.report(2, 1, started);
        assert!(!clock.is_idle());
        assert!(!clock.exit_due(&limits(Some(60), None), started + secs(600)));
    }

    #[test]
    fn lifetime_reached_exits_even_when_busy() {
        let started = Instant::now();
        let lifetime = limits(None, Some(600));
        let clock = ExitClock::new(started);
        clock.report(1, 2, started);
        assert!(!clock.exit_due(&lifetime, started + secs(599)));
        assert!(clock.exit_due(&lifetime, started + secs(600)));
        assert!(!clock.is_idle());
    }

    #[test]
    fn none_limits_never_exit() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        assert!(!clock.exit_due(&limits(None, None), started + secs(1_000_000)));
        assert_eq!(clock.deadline(&limits(None, None)), None);
    }

    #[test]
    fn disconnected_worker_exits_at_the_earliest_deadline() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        assert_eq!(
            clock.deadline(&limits(Some(60), Some(600))),
            Some(started + secs(60))
        );
        clock.report(1, 1, started);
        assert_eq!(
            clock.deadline(&limits(Some(60), Some(600))),
            Some(started + secs(600))
        );
    }

    #[test]
    fn a_disconnected_worker_with_a_running_job_is_not_idle() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        let (jobs, message_loop) = (new_session(), new_session());
        clock.report(jobs, 1, started);
        clock.report(message_loop, 0, started);
        assert_eq!(clock.deadline(&limits(Some(10), None)), None);
    }

    #[test]
    fn lifetime_left_counts_down_from_the_start() {
        let started = Instant::now();
        let clock = ExitClock::new(started);
        assert_eq!(
            clock.lifetime_left(&limits(None, Some(100)), started + secs(30)),
            Some(secs(70))
        );
        assert_eq!(
            clock.lifetime_left(&limits(Some(10), None), started + secs(30)),
            None
        );
    }
}
