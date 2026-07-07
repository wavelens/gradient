/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dual token-bucket limiter for worker→server log forwarding. One bucket
//! bounds a 1-minute burst, the other a 1-hour sustained rate. A log chunk is
//! forwarded only if BOTH buckets admit it; once either is exhausted the
//! limiter trips permanently for that build and forwarding stops. The build
//! itself keeps running - only the log stream is capped.

/// Per-build byte budgets for the two windows.
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
}

impl Bucket {
    fn new(capacity_per_window: u64, window_secs: f64) -> Self {
        let capacity = capacity_per_window as f64;
        Self {
            capacity,
            tokens: capacity,
            refill_per_sec: capacity / window_secs,
            last_t: 0.0,
        }
    }

    fn try_take(&mut self, n: f64, now: f64) -> bool {
        let dt = (now - self.last_t).max(0.0);
        self.tokens = (self.tokens + dt * self.refill_per_sec).min(self.capacity);
        self.last_t = now;
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
}

pub struct LogRateLimiter {
    minute: Bucket,
    hour: Bucket,
    tripped: bool,
}

impl LogRateLimiter {
    pub fn new(bytes_per_min: u64, bytes_per_hour: u64) -> Self {
        Self {
            minute: Bucket::new(bytes_per_min, 60.0),
            hour: Bucket::new(bytes_per_hour, 3600.0),
            tripped: false,
        }
    }

    pub fn from_limits(limits: LogRateLimits) -> Self {
        Self::new(limits.burst_bytes_per_min, limits.sustained_bytes_per_hour)
    }

    /// Whether `n` bytes may be forwarded at `now` (seconds since stream start).
    /// On the first denial the limiter trips permanently. When the two buckets
    /// disagree the admitting bucket has already been debited; harmless since we
    /// stop forwarding entirely after the trip.
    pub fn admit(&mut self, n: u64, now: f64) -> bool {
        if self.tripped {
            return false;
        }
        let n = n as f64;
        let m = self.minute.try_take(n, now);
        let h = self.hour.try_take(n, now);
        if m && h {
            true
        } else {
            self.tripped = true;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LogRateLimiter;

    #[test]
    fn allows_under_limit() {
        let mut l = LogRateLimiter::new(1000, 5000);
        assert!(l.admit(500, 0.0));
        assert!(l.admit(400, 0.0));
    }

    #[test]
    fn trips_on_burst() {
        let mut l = LogRateLimiter::new(1000, 5000);
        assert!(l.admit(1000, 0.0));
        assert!(!l.admit(1, 0.0));
    }

    #[test]
    fn refills_when_not_yet_tripped() {
        let mut l = LogRateLimiter::new(1000, 100_000);
        assert!(l.admit(1000, 0.0));
        // 30s later the minute bucket has refilled ~500 bytes
        assert!(l.admit(400, 30.0));
    }

    #[test]
    fn trips_on_sustained_even_when_burst_ok() {
        let mut l = LogRateLimiter::new(10_000, 5000);
        assert!(l.admit(5000, 0.0));
        assert!(!l.admit(1, 0.0));
    }
}
