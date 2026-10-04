/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::atomic::{AtomicU64, Ordering};

const ALPHA: f64 = 0.3;

/// Below this size the connection setup and round trips are dominating the elapsed time.
const MIN_TRANSFER_BYTES: u64 = 1024 * 1024;

pub static UPLOAD: ThroughputEwma = ThroughputEwma::new();
pub static DOWNLOAD: ThroughputEwma = ThroughputEwma::new();
pub static DISK: ThroughputEwma = ThroughputEwma::new();

/// The `0` bit pattern is marking "no sample yet".
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
        if bytes < MIN_TRANSFER_BYTES {
            return;
        }

        self.observe(bytes as f64 * 8.0 / elapsed.as_secs_f64().max(1e-6) / 1_000_000.0);
    }

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
        e.observe_transfer(4_000_000, std::time::Duration::from_secs(2));
        assert_eq!(e.current(), Some(16.0));
    }

    #[test]
    fn a_transfer_under_a_mebibyte_is_ignored() {
        let e = ThroughputEwma::new();
        e.observe_transfer(MIN_TRANSFER_BYTES - 1, std::time::Duration::from_millis(1));
        assert_eq!(e.current(), None);
    }

    #[test]
    fn non_positive_ignored() {
        let e = ThroughputEwma::new();
        e.observe(0.0);
        e.observe(-5.0);
        assert_eq!(e.current(), None);
    }
}
