/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The latest value reported per key, forgotten once its reporter falls silent
//! for longer than the TTL. Every `set` sweeps the stale entries, so the map
//! never outgrows the keys reported within one TTL.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

use crate::sync::Mutex;

#[derive(Debug)]
pub struct Latest<K, V> {
    entries: Mutex<HashMap<K, (Instant, V)>>,
    ttl: Duration,
}

impl<K: Eq + Hash, V: Clone> Latest<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn set(&self, key: K, value: V, now: Instant) {
        let mut entries = self.entries.lock();
        entries.retain(|_, (at, _)| now.duration_since(*at) < self.ttl);
        entries.insert(key, (now, value));
    }

    pub fn get(&self, key: &K, now: Instant) -> Option<V> {
        self.entries
            .lock()
            .get(key)
            .filter(|(at, _)| now.duration_since(*at) < self.ttl)
            .map(|(_, value)| value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(10);

    #[test]
    fn a_value_is_read_back_until_its_reporter_falls_silent() {
        let latest = Latest::new(TTL);
        let t0 = Instant::now();

        latest.set("a", 1, t0);
        latest.set("a", 2, t0 + Duration::from_secs(5));

        assert_eq!(latest.get(&"a", t0 + Duration::from_secs(14)), Some(2));
        assert_eq!(latest.get(&"a", t0 + Duration::from_secs(15)), None);
        assert_eq!(latest.get(&"b", t0), None);
    }

    #[test]
    fn a_set_sweeps_every_stale_key() {
        let latest = Latest::new(TTL);
        let t0 = Instant::now();

        latest.set("a", 1, t0);
        latest.set("b", 2, t0 + TTL);

        assert_eq!(latest.entries.lock().len(), 1);
    }
}
