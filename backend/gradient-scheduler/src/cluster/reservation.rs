/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use gradient_entity::ids::ClusterJobId;

use super::planner::plan;
use super::{ClusterSnapshot, PendingCluster, Placement, ScoreLookup, Slot, SlotKind};

#[derive(Debug, Clone)]
pub struct Reservation {
    pub placement: Placement,
    pub kinds: Vec<SlotKind>,
    pub since: Instant,
}

impl Reservation {
    pub fn cluster(&self) -> ClusterJobId {
        self.placement.cluster
    }

    pub fn seats(&self) -> impl Iterator<Item = (&str, SlotKind)> {
        self.placement
            .seats
            .iter()
            .zip(&self.kinds)
            .map(|(seat, kind)| (seat.worker.as_str(), *kind))
    }

    pub fn holds(&self, worker: &str, kind: SlotKind) -> bool {
        self.seats().any(|(w, k)| w == worker && k == kind)
    }

    pub fn seats_worker(&self, worker: &str) -> bool {
        self.seats().any(|(w, _)| w == worker)
    }
}

pub struct AgingPolicy {
    pub reserve_after: chrono::Duration,
    pub timeout: Duration,
}

pub enum AgingStep {
    Keep,
    Expire,
    Reserve(Reservation),
    Commit(Placement),
}

pub fn aging_step(
    snapshot: &ClusterSnapshot,
    now: NaiveDateTime,
    at: Instant,
    policy: &AgingPolicy,
) -> AgingStep {
    match &snapshot.reservation {
        Some(held) => held_step(snapshot, held, now, at, policy),
        None => reserve_step(snapshot, now, at, policy),
    }
}

pub fn hide_reserved(snapshot: &mut ClusterSnapshot) {
    if let Some(held) = snapshot.reservation.clone() {
        snapshot.slots.retain(|s| !held.holds(&s.worker, s.kind));
    }
}

fn held_step(
    snapshot: &ClusterSnapshot,
    held: &Reservation,
    now: NaiveDateTime,
    at: Instant,
    policy: &AgingPolicy,
) -> AgingStep {
    let Some(cluster) = snapshot.clusters.iter().find(|c| c.id == held.cluster()) else {
        return AgingStep::Expire;
    };
    if !held.seats().all(|seat| has_slot(&snapshot.connected, seat)) {
        return AgingStep::Expire;
    }
    if at.duration_since(held.since) >= policy.timeout {
        return match reserve_step(snapshot, now, at, policy) {
            AgingStep::Reserve(renewed) => AgingStep::Reserve(renewed),
            _ => AgingStep::Expire,
        };
    }
    if !held.seats().all(|seat| has_slot(&snapshot.slots, seat)) {
        return AgingStep::Keep;
    }

    let seats: Vec<Slot> = snapshot
        .slots
        .iter()
        .filter(|s| held.seats_worker(&s.worker))
        .cloned()
        .collect();
    match plan(cluster, &seats, &snapshot.scores) {
        Some(placement) => AgingStep::Commit(placement),
        None => AgingStep::Expire,
    }
}

fn reserve_step(
    snapshot: &ClusterSnapshot,
    now: NaiveDateTime,
    at: Instant,
    policy: &AgingPolicy,
) -> AgingStep {
    let Some(cluster) = oldest_aged(snapshot, now, policy) else {
        return AgingStep::Keep;
    };
    if plan(cluster, &snapshot.slots, &snapshot.scores).is_some() {
        return AgingStep::Keep;
    }

    match plan(cluster, &snapshot.connected, &idle_first(snapshot, cluster)) {
        Some(placement) => AgingStep::Reserve(Reservation {
            kinds: placement
                .seats
                .iter()
                .map(|seat| PendingCluster::slot_kind(&cluster.members[seat.member]))
                .collect(),
            placement,
            since: at,
        }),
        None => AgingStep::Keep,
    }
}

fn oldest_aged<'a>(
    snapshot: &'a ClusterSnapshot,
    now: NaiveDateTime,
    policy: &AgingPolicy,
) -> Option<&'a PendingCluster> {
    snapshot
        .clusters
        .iter()
        .filter(|c| c.ready() && now - c.queued_at >= policy.reserve_after)
        .min_by_key(|c| (!c.prioritized(), c.queued_at))
}

/// An idle worker is keeping its score and a busy one is counting as unscored. The planner is then
/// seating idle workers wherever a full match is possible.
fn idle_first(snapshot: &ClusterSnapshot, cluster: &PendingCluster) -> ScoreLookup {
    let idle: HashSet<&str> = snapshot.slots.iter().map(|s| s.worker.as_str()).collect();
    let mut scores = ScoreLookup::new();
    for worker in &idle {
        for member in &cluster.members {
            let key = ((*worker).to_owned(), member.key.clone());
            let score = snapshot.scores.get(&key).cloned().unwrap_or_default();
            scores.insert(key, score);
        }
    }

    scores
}

fn has_slot(slots: &[Slot], (worker, kind): (&str, SlotKind)) -> bool {
    slots.iter().any(|s| s.worker == worker && s.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ClusterMember;
    use crate::jobs::PendingJob;
    use crate::scheduler_tests::eval_job;
    use gradient_entity::ids::{ClusterJobId, ClusterMemberId};
    use gradient_pool::WorkerCaps;
    use gradient_types::ids::ProjectId;
    use gradient_wire::types::GradientCapabilities;

    fn slot(worker: &str) -> Slot {
        Slot {
            worker: worker.into(),
            kind: SlotKind::Eval,
            zone: None,
            caps: WorkerCaps {
                capabilities: GradientCapabilities {
                    eval: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            authorized: None,
        }
    }

    fn cluster(members: usize, age_secs: i64, prioritized: bool) -> PendingCluster {
        let peer = ProjectId::now_v7();
        PendingCluster {
            id: ClusterJobId::now_v7(),
            same_zone: false,
            queued_at: gradient_types::now() - chrono::Duration::seconds(age_secs),
            expected: members,
            members: (0..members)
                .map(|i| ClusterMember {
                    id: ClusterMemberId::now_v7(),
                    role: format!("r{i}"),
                    primary: false,
                    pin: None,
                    key: format!("eval:{i}"),
                    job: Some(PendingJob::Eval(crate::jobs::PendingEvalJob {
                        prioritized,
                        ..eval_job(peer)
                    })),
                })
                .collect(),
            not_before: None,
        }
    }

    fn snapshot(
        clusters: Vec<PendingCluster>,
        idle: &[&str],
        connected: &[&str],
    ) -> ClusterSnapshot {
        ClusterSnapshot {
            clusters,
            slots: idle.iter().map(|w| slot(w)).collect(),
            connected: connected.iter().map(|w| slot(w)).collect(),
            scores: Default::default(),
            reservation: None,
        }
    }

    fn policy() -> AgingPolicy {
        AgingPolicy {
            reserve_after: chrono::Duration::seconds(600),
            timeout: Duration::from_secs(1800),
        }
    }

    fn step(snapshot: &ClusterSnapshot) -> AgingStep {
        aging_step(snapshot, gradient_types::now(), Instant::now(), &policy())
    }

    fn reserved(step: AgingStep) -> Reservation {
        match step {
            AgingStep::Reserve(r) => r,
            _ => panic!("expected a reservation"),
        }
    }

    #[test]
    fn a_young_cluster_reserves_nothing() {
        let s = snapshot(vec![cluster(2, 60, false)], &["w1"], &["w1", "w2"]);

        assert!(matches!(step(&s), AgingStep::Keep));
    }

    #[test]
    fn an_old_cluster_reserves_idle_workers_first() {
        let s = snapshot(vec![cluster(2, 700, false)], &["w3"], &["w1", "w2", "w3"]);

        let r = reserved(step(&s));

        assert!(r.seats_worker("w3"));
        assert_eq!(r.seats().count(), 2);
    }

    #[test]
    fn an_old_cluster_without_enough_workers_reserves_nothing() {
        let s = snapshot(vec![cluster(3, 700, false)], &[], &["w1", "w2"]);

        assert!(matches!(step(&s), AgingStep::Keep));
    }

    #[test]
    fn the_prioritized_cluster_reserves_first() {
        let old = cluster(2, 900, false);
        let urgent = cluster(2, 700, true);
        let urgent_id = urgent.id;
        let s = snapshot(vec![old, urgent], &[], &["w1", "w2"]);

        assert_eq!(reserved(step(&s)).cluster(), urgent_id);
    }

    #[test]
    fn a_reservation_commits_once_every_seat_is_idle() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        let r = reserved(step(&s));
        s.reservation = Some(r);
        s.slots = vec![slot("w1"), slot("w2")];

        assert!(matches!(step(&s), AgingStep::Commit(_)));
    }

    #[test]
    fn a_reservation_waits_while_a_seat_is_busy() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        s.reservation = Some(reserved(step(&s)));
        s.slots = vec![slot("w1")];

        assert!(matches!(step(&s), AgingStep::Keep));
    }

    #[test]
    fn a_held_reservation_blocks_a_second() {
        let first = cluster(2, 800, false);
        let mut s = snapshot(
            vec![first, cluster(2, 700, false)],
            &[],
            &["w1", "w2", "w3", "w4"],
        );
        s.reservation = Some(reserved(step(&s)));

        assert!(matches!(step(&s), AgingStep::Keep));
    }

    #[test]
    fn an_old_reservation_is_planned_again() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        let mut r = reserved(step(&s));
        r.since = Instant::now() - Duration::from_secs(1801);
        s.reservation = Some(r);

        let AgingStep::Reserve(renewed) = step(&s) else {
            panic!("a timed-out reservation is planned again");
        };
        assert!(Instant::now().duration_since(renewed.since) < Duration::from_secs(1));
    }

    #[test]
    fn a_seat_that_can_no_longer_run_its_member_expires_the_reservation() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        s.reservation = Some(reserved(step(&s)));
        let mut unfit = slot("w1");
        unfit.caps.capabilities.eval = false;
        s.slots = vec![unfit, slot("w2")];

        assert!(matches!(step(&s), AgingStep::Expire));
    }

    #[test]
    fn a_reservation_with_a_lost_seat_expires() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        s.reservation = Some(reserved(step(&s)));
        s.connected = vec![slot("w1")];

        assert!(matches!(step(&s), AgingStep::Expire));
    }

    #[test]
    fn a_reservation_for_a_vanished_cluster_expires() {
        let c = cluster(2, 700, false);
        let mut s = snapshot(vec![c], &[], &["w1", "w2"]);
        s.reservation = Some(reserved(step(&s)));
        s.clusters.clear();

        assert!(matches!(step(&s), AgingStep::Expire));
    }

    #[test]
    fn seats_reserved_for_another_cluster_are_hidden_from_planning() {
        let old = cluster(2, 700, false);
        let young = cluster(2, 10, false);
        let young_id = young.id;
        let mut s = snapshot(vec![old, young], &[], &["w1", "w2"]);
        s.reservation = Some(reserved(step(&s)));
        s.slots = vec![slot("w1"), slot("w2"), slot("w3")];

        hide_reserved(&mut s);

        let open: Vec<&str> = s.slots.iter().map(|x| x.worker.as_str()).collect();
        assert_eq!(open, ["w3"]);
        let young = s.clusters.iter().find(|c| c.id == young_id).expect("young");
        assert!(plan(young, &s.slots, &s.scores).is_none());
    }
}
