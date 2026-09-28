/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Thread-safe EWMA accumulators for passively-measured worker throughput.
//! `NETWORK` (Mbps, from relayed and presigned NAR transfers) and `DISK` (MB/s, from per-build cgroup
//! io.stat) are read by the heartbeat and reported via `WorkerMetrics`.

use std::sync::atomic::{AtomicU64, Ordering};

const ALPHA: f64 = 0.3;

pub static NETWORK: ThroughputEwma = ThroughputEwma::new();
pub static DISK: ThroughputEwma = ThroughputEwma::new();

/// EWMA over a positive scalar rate. The `0` bit pattern means "no sample yet".
#[derive(Debug)]
pub struct ThroughputEwma {
    bits: AtomicU64,
}

impl ThroughputEwma {
    pub const fn new() -> Self {
        Self {
            bits: AtomicU64::new(0),
        }
    }

    /// Fold one positive observation into the EWMA. Non-positive values are ignored.
    pub fn observe(&self, value: f64) {
        if !value.is_finite() || value <= 0.0 {
            return;
        }
        loop {
            let prev = self.bits.load(Ordering::Relaxed);
            let next = if prev == 0 {
                value
            } else {
                ALPHA * value + (1.0 - ALPHA) * f64::from_bits(prev)
            };
            if self
                .bits
                .compare_exchange_weak(prev, next.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn observe_transfer(&self, bytes: u64, elapsed: std::time::Duration) {
        self.observe(bytes as f64 * 8.0 / elapsed.as_secs_f64().max(1e-6) / 1_000_000.0);
    }

    /// Current EWMA, or `None` until the first observation.
    pub fn current(&self) -> Option<f32> {
        match self.bits.load(Ordering::Relaxed) {
            0 => None,
            b => Some(f64::from_bits(b) as f32),
        }
    }
}

impl Default for ThroughputEwma {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_none() {
        assert_eq!(ThroughputEwma::new().current(), None);
    }

    #[test]
    fn first_sample_sets_value() {
        let e = ThroughputEwma::new();
        e.observe(100.0);
        assert_eq!(e.current(), Some(100.0));
    }

    #[test]
    fn converges_toward_steady_state() {
        let e = ThroughputEwma::new();
        e.observe(100.0);
        for _ in 0..50 {
            e.observe(200.0);
        }
        let v = e.current().unwrap();
        assert!(v > 190.0 && v <= 200.0, "expected near 200, got {v}");
    }

    #[test]
    fn a_transfer_is_observed_in_megabits_per_second() {
        let e = ThroughputEwma::new();
        e.observe_transfer(1_000_000, std::time::Duration::from_secs(1));
        assert_eq!(e.current(), Some(8.0));
    }

    #[test]
    fn non_positive_ignored() {
        let e = ThroughputEwma::new();
        e.observe(0.0);
        e.observe(-5.0);
        assert_eq!(e.current(), None);
    }
}
