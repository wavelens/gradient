/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One event per key per interval, decided in memory. Entries are never
//! evicted: the key spaces using this are bounded by small tables.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

use crate::sync::Mutex;

#[derive(Debug)]
pub struct Debounce<K> {
    seen: Mutex<HashMap<K, Instant>>,
    interval: Duration,
}

impl<K: Eq + Hash> Debounce<K> {
    pub fn new(interval: Duration) -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            interval,
        }
    }

    /// True when `key` has not fired within `interval` before `now`; records `now` when it is.
    pub fn due(&self, key: K, now: Instant) -> bool {
        let mut seen = self.seen.lock();

        match seen.get(&key) {
            Some(last) if now.duration_since(*last) < self.interval => false,
            _ => {
                seen.insert(key, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_call_per_key_is_due_and_repeats_inside_the_interval_are_not() {
        let d = Debounce::new(Duration::from_secs(60));
        let t0 = Instant::now();

        assert!(d.due("a", t0));
        assert!(!d.due("a", t0 + Duration::from_secs(59)));
        assert!(d.due("b", t0 + Duration::from_secs(1)));
        assert!(d.due("a", t0 + Duration::from_secs(60)));
        assert!(!d.due("a", t0 + Duration::from_secs(119)));
    }

    #[test]
    fn a_zero_interval_is_always_due() {
        let d = Debounce::new(Duration::ZERO);
        let t0 = Instant::now();

        assert!(d.due(1, t0));
        assert!(d.due(1, t0));
    }
}
