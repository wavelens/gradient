/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::time::{Duration, Instant};

use crate::sync::Mutex;

#[derive(Debug)]
pub struct MemberSums<K, A> {
    entries: Mutex<HashMap<K, Members<A>>>,
    ttl: Duration,
}

#[derive(Debug)]
struct Members<A> {
    attempt: A,
    counts: HashMap<String, u64>,
    at: Instant,
}

impl<K: Eq + Hash, A: PartialEq> MemberSums<K, A> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn record(&self, key: K, attempt: A, member: &str, count: u64, now: Instant) -> u64 {
        let mut entries = self.entries.lock();
        entries.retain(|_, members| now.duration_since(members.at) < self.ttl);
        let members = match entries.entry(key) {
            Entry::Occupied(occupied) => {
                let members = occupied.into_mut();
                if members.attempt != attempt {
                    members.attempt = attempt;
                    members.counts.clear();
                }
                members
            }
            Entry::Vacant(vacant) => vacant.insert(Members {
                attempt,
                counts: HashMap::new(),
                at: now,
            }),
        };
        members.at = now;
        members.counts.insert(member.to_owned(), count);
        members.counts.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn a_member_that_stops_reporting_stays_in_the_sum_while_others_report() {
        let sums = MemberSums::new(TTL);
        let t0 = Instant::now();

        sums.record("e", 1, "a", 10, t0);
        sums.record("e", 1, "b", 5, t0 + Duration::from_secs(1));

        assert_eq!(sums.record("e", 1, "b", 7, t0 + TTL), 17);
    }

    #[test]
    fn a_new_attempt_starts_from_its_own_counts() {
        let sums = MemberSums::new(TTL);
        let t0 = Instant::now();

        sums.record("e", 1, "a", 10, t0);

        assert_eq!(sums.record("e", 2, "c", 3, t0), 3);
    }

    #[test]
    fn a_silent_key_is_swept() {
        let sums = MemberSums::new(TTL);
        let t0 = Instant::now();

        sums.record("e", 1, "a", 10, t0);
        sums.record("f", 1, "a", 1, t0 + TTL);

        assert_eq!(sums.entries.lock().len(), 1);
    }
}
