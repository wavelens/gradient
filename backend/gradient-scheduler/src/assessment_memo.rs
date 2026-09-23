/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The build-phase assessment of each evaluation, kept while nothing it read
//! can have moved: the evaluation's counters and the connected pool. Every
//! other input (a relay flip, a feature edge) is re-read after [`AssessmentMemo::TTL`].

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use gradient_db::EvalCounters;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;

pub(crate) type Caps = [(Vec<String>, Vec<String>)];

struct Entry {
    key: (EvalCounters, u64),
    at: Instant,
    target: EvaluationStatus,
    reason: Option<WaitingReason>,
}

#[derive(Default)]
pub(crate) struct AssessmentMemo {
    entries: HashMap<EvaluationId, Entry>,
}

impl AssessmentMemo {
    pub(crate) const TTL: Duration = Duration::from_secs(60);

    pub(crate) fn get(
        &self,
        evaluation: EvaluationId,
        counters: EvalCounters,
        caps: &Caps,
        now: Instant,
    ) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
        let entry = self.entries.get(&evaluation)?;
        let fresh = now.saturating_duration_since(entry.at) < Self::TTL;
        (fresh && entry.key == (counters, caps_fingerprint(caps)))
            .then(|| (entry.target, entry.reason.clone()))
    }

    pub(crate) fn put(
        &mut self,
        evaluation: EvaluationId,
        counters: EvalCounters,
        caps: &Caps,
        now: Instant,
        target: EvaluationStatus,
        reason: Option<WaitingReason>,
    ) {
        self.entries.insert(
            evaluation,
            Entry {
                key: (counters, caps_fingerprint(caps)),
                at: now,
                target,
                reason,
            },
        );
    }

    pub(crate) fn retain(&mut self, live: &HashSet<EvaluationId>) {
        self.entries.retain(|e, _| live.contains(e));
    }
}

/// The pool as a set: worker order is connection order and decides nothing.
pub(crate) fn caps_fingerprint(caps: &Caps) -> u64 {
    let mut workers: Vec<(Vec<&str>, Vec<&str>)> = caps
        .iter()
        .map(|(archs, feats)| {
            let mut archs: Vec<&str> = archs.iter().map(String::as_str).collect();
            let mut feats: Vec<&str> = feats.iter().map(String::as_str).collect();
            archs.sort_unstable();
            feats.sort_unstable();
            (archs, feats)
        })
        .collect();
    workers.sort_unstable();

    let mut hasher = DefaultHasher::new();
    workers.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Vec<(Vec<String>, Vec<String>)> {
        vec![(vec!["x86_64-linux".to_owned()], vec![])]
    }

    #[test]
    fn a_hit_needs_the_same_counters_and_pool_within_the_ttl() {
        let mut memo = AssessmentMemo::default();
        let e = EvaluationId::now_v7();
        let c = EvalCounters {
            named: 3,
            active: 2,
            queued: 1,
            ..Default::default()
        };
        let t0 = Instant::now();
        memo.put(e, c, &caps(), t0, EvaluationStatus::Waiting, None);

        assert!(
            memo.get(e, c, &caps(), t0 + Duration::from_secs(5))
                .is_some()
        );
        assert!(
            memo.get(e, EvalCounters { queued: 2, ..c }, &caps(), t0)
                .is_none(),
            "a counter move re-assesses"
        );
        assert!(
            memo.get(e, c, &[], t0).is_none(),
            "a pool change re-assesses"
        );
        assert!(
            memo.get(e, c, &caps(), t0 + AssessmentMemo::TTL).is_none(),
            "a flip no counter sees is re-read within the ttl"
        );
    }

    #[test]
    fn the_fingerprint_ignores_worker_order() {
        let a = vec![
            (vec!["a".to_owned()], vec!["kvm".to_owned()]),
            (vec!["b".to_owned()], vec![]),
        ];
        let b = vec![a[1].clone(), a[0].clone()];
        assert_eq!(caps_fingerprint(&a), caps_fingerprint(&b));
        assert_ne!(caps_fingerprint(&a), caps_fingerprint(&a[..1]));
    }

    #[test]
    fn retain_drops_finished_evaluations() {
        let mut memo = AssessmentMemo::default();
        let e = EvaluationId::now_v7();
        let now = Instant::now();
        memo.put(
            e,
            EvalCounters::default(),
            &[],
            now,
            EvaluationStatus::Building,
            None,
        );
        memo.retain(&HashSet::new());
        assert!(memo.get(e, EvalCounters::default(), &[], now).is_none());
    }
}
