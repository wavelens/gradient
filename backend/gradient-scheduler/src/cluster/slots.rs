/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gradient_pool::WorkerCaps;
use gradient_types::ids::ProjectId;
use gradient_wire::types::JobKind;

use super::PendingCluster;
use crate::jobs::WorkerJobScore;

/// Two 10 s worker heartbeats plus slack: an idle worker re-asks within it.
pub const IDLE_SLOT_TTL: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    Eval,
    Build,
}

impl From<&JobKind> for SlotKind {
    fn from(kind: &JobKind) -> Self {
        match kind {
            JobKind::Flake => Self::Eval,
            JobKind::Build => Self::Build,
        }
    }
}

#[derive(Debug, Default)]
pub struct IdleSlots {
    seen: HashMap<(String, SlotKind), Instant>,
}

impl IdleSlots {
    pub fn record(&mut self, worker: &str, kind: SlotKind, now: Instant) {
        self.seen.insert((worker.to_owned(), kind), now);
    }

    pub fn clear(&mut self, worker: &str, kind: SlotKind) {
        self.seen.remove(&(worker.to_owned(), kind));
    }

    pub fn forget_worker(&mut self, worker: &str) {
        self.seen.retain(|(w, _), _| w != worker);
    }

    pub fn live(&self, now: Instant) -> impl Iterator<Item = (&str, SlotKind)> {
        self.seen
            .iter()
            .filter(move |(_, seen)| now.duration_since(**seen) <= IDLE_SLOT_TTL)
            .map(|((worker, kind), _)| (worker.as_str(), *kind))
    }
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub worker: String,
    pub kind: SlotKind,
    pub zone: Option<String>,
    pub caps: WorkerCaps,
    pub authorized: Option<HashSet<ProjectId>>,
}

#[derive(Debug, Clone, Default)]
pub struct ClusterSnapshot {
    pub clusters: Vec<PendingCluster>,
    pub slots: Vec<Slot>,
    pub scores: HashMap<(String, String), WorkerJobScore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_older_than_the_ttl_is_not_live() {
        let start = Instant::now();
        let mut idle = IdleSlots::default();
        idle.record("w1", SlotKind::Build, start);
        idle.record("w2", SlotKind::Eval, start + IDLE_SLOT_TTL);

        let later = start + IDLE_SLOT_TTL + Duration::from_secs(1);
        let live: Vec<_> = idle.live(later).collect();

        assert_eq!(live, vec![("w2", SlotKind::Eval)]);
    }

    #[test]
    fn a_worker_leaving_takes_all_its_slots() {
        let now = Instant::now();
        let mut idle = IdleSlots::default();
        idle.record("w1", SlotKind::Build, now);
        idle.record("w1", SlotKind::Eval, now);
        idle.record("w2", SlotKind::Eval, now);

        idle.forget_worker("w1");

        assert_eq!(
            idle.live(now).collect::<Vec<_>>(),
            vec![("w2", SlotKind::Eval)]
        );
    }
}
