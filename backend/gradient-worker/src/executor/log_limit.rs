/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, Clone, Copy)]
pub struct LogRateLimits {
    pub burst_bytes_per_min: u64,
    pub sustained_bytes_per_hour: u64,
}

struct Bucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_t: f64,
    resume_at: f64,
}

impl Bucket {
    fn new(capacity_per_window: u64, window_secs: f64, resume_at: f64) -> Self {
        let capacity = capacity_per_window as f64;
        Self {
            capacity,
            tokens: capacity,
            refill_per_sec: capacity / window_secs,
            last_t: 0.0,
            resume_at: resume_at.min(capacity),
        }
    }

    fn refill(&mut self, now: f64) {
        let dt = (now - self.last_t).max(0.0);
        self.tokens = (self.tokens + dt * self.refill_per_sec).min(self.capacity);
        self.last_t = now;
    }

    fn holds(&self, n: f64, paused: bool) -> bool {
        let needed = if paused { n.max(self.resume_at) } else { n };
        self.tokens >= needed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Forward,
    Paused,
    Skipped,
    Resumed { skipped_lines: u64 },
}

pub struct LogRateLimiter {
    minute: Bucket,
    hour: Bucket,
    skipped_lines: Option<u64>,
}

impl From<LogRateLimits> for LogRateLimiter {
    fn from(limits: LogRateLimits) -> Self {
        Self::new(limits.burst_bytes_per_min, limits.sustained_bytes_per_hour)
    }
}

impl LogRateLimiter {
    pub fn new(bytes_per_min: u64, bytes_per_hour: u64) -> Self {
        let resume_at = bytes_per_min as f64 / 4.0;
        Self {
            minute: Bucket::new(bytes_per_min, 60.0, resume_at),
            hour: Bucket::new(bytes_per_hour, 3600.0, resume_at),
            skipped_lines: None,
        }
    }

    /// A paused log can stay paused until both buckets hold a quarter of the per-minute budget again. A bucket with a smaller capacity than that value can fill instead. The log can then continue as a burst, not as a sparse sample of lines.
    pub fn admit(&mut self, n: u64, now: f64) -> Admission {
        let n = n as f64;
        self.minute.refill(now);
        self.hour.refill(now);
        let paused = self.skipped_lines.is_some();
        if !self.minute.holds(n, paused) || !self.hour.holds(n, paused) {
            let pausing = self.skipped_lines.is_none();
            *self.skipped_lines.get_or_insert(0) += 1;
            return if pausing {
                Admission::Paused
            } else {
                Admission::Skipped
            };
        }
        self.minute.tokens -= n;
        self.hour.tokens -= n;
        match self.skipped_lines.take() {
            Some(skipped_lines) => Admission::Resumed { skipped_lines },
            None => Admission::Forward,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Admission, LogRateLimiter};

    #[test]
    fn allows_under_limit() {
        let mut l = LogRateLimiter::new(1000, 5000);
        assert_eq!(l.admit(500, 0.0), Admission::Forward);
        assert_eq!(l.admit(400, 0.0), Admission::Forward);
    }

    #[test]
    fn pauses_on_burst_and_skips_until_refilled() {
        let mut l = LogRateLimiter::new(1000, 5000);
        assert_eq!(l.admit(1000, 0.0), Admission::Forward);
        assert_eq!(l.admit(1, 0.0), Admission::Paused);
        assert_eq!(l.admit(1, 1.0), Admission::Skipped);
    }

    #[test]
    fn refills_while_under_limit() {
        let mut l = LogRateLimiter::new(1000, 100_000);
        assert_eq!(l.admit(1000, 0.0), Admission::Forward);
        assert_eq!(l.admit(400, 30.0), Admission::Forward);
    }

    #[test]
    fn pauses_on_sustained_even_when_burst_ok() {
        let mut l = LogRateLimiter::new(10_000, 5000);
        assert_eq!(l.admit(5000, 0.0), Admission::Forward);
        assert_eq!(l.admit(1, 0.0), Admission::Paused);
    }

    #[test]
    fn resumes_with_the_skipped_count_once_a_quarter_minute_refilled() {
        let mut l = LogRateLimiter::new(1200, 100_000);
        assert_eq!(l.admit(1200, 0.0), Admission::Forward);
        assert_eq!(l.admit(10, 0.0), Admission::Paused);
        assert_eq!(l.admit(10, 10.0), Admission::Skipped);
        assert_eq!(l.admit(10, 15.0), Admission::Resumed { skipped_lines: 2 });
        assert_eq!(l.admit(10, 15.0), Admission::Forward);
    }

    #[test]
    fn resumes_when_the_hour_budget_is_below_a_quarter_minute() {
        let mut l = LogRateLimiter::new(4000, 360);
        assert_eq!(l.admit(360, 0.0), Admission::Forward);
        assert_eq!(l.admit(10, 0.0), Admission::Paused);
        assert_eq!(l.admit(10, 3600.0), Admission::Resumed { skipped_lines: 1 });
    }

    #[test]
    fn a_denied_line_debits_no_bucket() {
        let mut l = LogRateLimiter::new(10_000, 5000);
        assert_eq!(l.admit(6000, 0.0), Admission::Paused);
        assert_eq!(l.admit(5000, 0.0), Admission::Resumed { skipped_lines: 1 });
    }
}
