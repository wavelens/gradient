/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Places one ready cluster on idle slots: every member on its own worker, all in
//! one zone when the cluster asks for it, cheapest locality first.

use std::collections::{BTreeMap, HashMap};

use gradient_types::ids::ClusterJobId;

use super::matching::kuhn;
use super::{ClusterMember, PendingCluster, Slot};
use crate::jobs::{WorkerJobScore, visible_to};

pub type ScoreLookup = HashMap<(String, String), WorkerJobScore>;

const UNSCORED_COST: u128 = u64::MAX as u128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub member: usize,
    pub worker: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub cluster: ClusterJobId,
    pub seats: Vec<Seat>,
}

pub fn plan(cluster: &PendingCluster, slots: &[Slot], scores: &ScoreLookup) -> Option<Placement> {
    zone_groups(cluster.same_zone, slots)
        .into_iter()
        .filter_map(|(zone, group)| {
            place_in(cluster, &group, scores).map(|(cost, seats)| (cost, zone, seats))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .map(|(_, _, seats)| Placement {
            cluster: cluster.id,
            seats,
        })
}

fn zone_groups(same_zone: bool, slots: &[Slot]) -> BTreeMap<Option<String>, Vec<&Slot>> {
    let mut groups: BTreeMap<Option<String>, Vec<&Slot>> = BTreeMap::new();
    for slot in slots {
        let zone = if same_zone { slot.zone.clone() } else { None };
        groups.entry(zone).or_default().push(slot);
    }

    groups
}

fn place_in(
    cluster: &PendingCluster,
    group: &[&Slot],
    scores: &ScoreLookup,
) -> Option<(u128, Vec<Seat>)> {
    let mut workers: Vec<&str> = group.iter().map(|s| s.worker.as_str()).collect();
    workers.sort_unstable();
    workers.dedup();

    let eligible: Vec<Vec<usize>> = cluster
        .members
        .iter()
        .map(|m| candidates(m, group, &workers, scores))
        .collect();
    let mut order: Vec<usize> = (0..eligible.len()).collect();
    order.sort_by_key(|&i| (cluster.members[i].pin.is_none(), eligible[i].len()));
    let ordered: Vec<Vec<usize>> = order.iter().map(|&i| eligible[i].clone()).collect();

    let matched = kuhn(&ordered)?;
    let seats: Vec<Seat> = order
        .iter()
        .zip(matched)
        .map(|(&member, w)| Seat {
            member,
            worker: workers[w].to_owned(),
        })
        .collect();
    let cost = seats
        .iter()
        .map(|s| seat_cost(&cluster.members[s.member], &s.worker, scores))
        .sum();

    Some((cost, seats))
}

fn candidates(
    member: &ClusterMember,
    group: &[&Slot],
    workers: &[&str],
    scores: &ScoreLookup,
) -> Vec<usize> {
    let Some(job) = &member.job else {
        return Vec::new();
    };
    let kind = PendingCluster::slot_kind(member);
    let mut fit: Vec<usize> = workers
        .iter()
        .enumerate()
        .filter(|(_, w)| member.pin.as_deref().is_none_or(|pin| pin == **w))
        .filter(|(_, w)| {
            group.iter().any(|s| {
                s.worker == **w
                    && s.kind == kind
                    && visible_to(job, s.authorized.as_ref(), Some(&s.caps))
            })
        })
        .map(|(i, _)| i)
        .collect();
    fit.sort_by_key(|&i| (seat_cost(member, workers[i], scores), i));

    fit
}

fn seat_cost(member: &ClusterMember, worker: &str, scores: &ScoreLookup) -> u128 {
    scores
        .get(&(worker.to_owned(), member.key.clone()))
        .map_or(UNSCORED_COST, |s| u128::from(s.missing_nar_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{ClusterMember, PendingCluster, Slot, SlotKind};
    use crate::jobs::{PendingJob, WorkerJobScore};
    use gradient_pool::WorkerCaps;
    use gradient_types::ids::{
        ClusterJobId, ClusterMemberId, DerivationBuildId, EvaluationId, ProjectId,
    };

    fn build_member(role: &str, pin: Option<&str>) -> ClusterMember {
        let anchor = DerivationBuildId::now_v7();
        let job =
            crate::scheduler_tests::build_job(EvaluationId::now_v7(), ProjectId::now_v7(), anchor);
        ClusterMember {
            id: ClusterMemberId::now_v7(),
            role: role.into(),
            primary: false,
            pin: pin.map(str::to_owned),
            key: crate::jobs::build_job_key(anchor),
            job: Some(PendingJob::Build(job)),
        }
    }

    fn cluster(same_zone: bool, members: Vec<ClusterMember>) -> PendingCluster {
        PendingCluster {
            id: ClusterJobId::now_v7(),
            same_zone,
            queued_at: gradient_types::now(),
            expected: members.len(),
            members,
            not_before: None,
        }
    }

    fn slot(worker: &str, zone: Option<&str>, arch: &str) -> Slot {
        Slot {
            worker: worker.into(),
            kind: SlotKind::Build,
            zone: zone.map(str::to_owned),
            caps: WorkerCaps {
                architectures: vec![arch.into()],
                ..WorkerCaps::default()
            },
            authorized: None,
        }
    }

    fn workers(placement: &Placement) -> Vec<&str> {
        let mut seats: Vec<_> = placement.seats.iter().collect();
        seats.sort_by_key(|s| s.member);
        seats.iter().map(|s| s.worker.as_str()).collect()
    }

    #[test]
    fn a_pinned_member_sits_on_its_worker() {
        let c = cluster(
            false,
            vec![build_member("a", None), build_member("b", Some("w1"))],
        );
        let slots = [
            slot("w1", None, "x86_64-linux"),
            slot("w2", None, "x86_64-linux"),
        ];

        let placement = plan(&c, &slots, &ScoreLookup::new()).expect("placed");

        assert_eq!(workers(&placement), vec!["w2", "w1"]);
    }

    #[test]
    fn a_worker_lacking_the_architecture_is_never_a_seat() {
        let c = cluster(
            false,
            vec![build_member("a", None), build_member("b", None)],
        );
        let slots = [
            slot("w1", None, "x86_64-linux"),
            slot("w2", None, "aarch64-linux"),
        ];

        assert!(plan(&c, &slots, &ScoreLookup::new()).is_none());
    }

    #[test]
    fn two_idle_kinds_on_one_worker_still_seat_one_member() {
        let c = cluster(
            false,
            vec![build_member("a", None), build_member("b", None)],
        );
        let mut eval = slot("w1", None, "x86_64-linux");
        eval.kind = SlotKind::Eval;
        let slots = [slot("w1", None, "x86_64-linux"), eval];

        assert!(plan(&c, &slots, &ScoreLookup::new()).is_none());
    }

    #[test]
    fn a_same_zone_cluster_stays_inside_one_zone() {
        let c = cluster(true, vec![build_member("a", None), build_member("b", None)]);
        let slots = [
            slot("w1", Some("a"), "x86_64-linux"),
            slot("w2", Some("b"), "x86_64-linux"),
            slot("w3", Some("b"), "x86_64-linux"),
        ];

        let placement = plan(&c, &slots, &ScoreLookup::new()).expect("placed");

        let mut seated = workers(&placement);
        seated.sort();
        assert_eq!(seated, vec!["w2", "w3"]);
    }

    #[test]
    fn workers_without_a_zone_form_one_zone() {
        let c = cluster(true, vec![build_member("a", None), build_member("b", None)]);
        let slots = [
            slot("w1", None, "x86_64-linux"),
            slot("w2", None, "x86_64-linux"),
            slot("w3", Some("a"), "x86_64-linux"),
        ];

        let placement = plan(&c, &slots, &ScoreLookup::new()).expect("placed");

        let mut seated = workers(&placement);
        seated.sort();
        assert_eq!(seated, vec!["w1", "w2"]);
    }

    #[test]
    fn a_cluster_free_to_span_zones_uses_every_slot() {
        let c = cluster(
            false,
            vec![build_member("a", None), build_member("b", None)],
        );
        let slots = [
            slot("w1", Some("a"), "x86_64-linux"),
            slot("w2", Some("b"), "x86_64-linux"),
        ];

        assert!(plan(&c, &slots, &ScoreLookup::new()).is_some());
    }

    #[test]
    fn the_zone_with_less_to_fetch_wins() {
        let a = build_member("a", None);
        let b = build_member("b", None);
        let mut scores = ScoreLookup::new();
        for (worker, cost) in [("w1", 900), ("w2", 900), ("w3", 10), ("w4", 10)] {
            for key in [&a.key, &b.key] {
                scores.insert(
                    (worker.into(), key.clone()),
                    WorkerJobScore {
                        missing_nar_size: cost,
                        ..WorkerJobScore::default()
                    },
                );
            }
        }
        let c = cluster(true, vec![a, b]);
        let slots = [
            slot("w1", Some("a"), "x86_64-linux"),
            slot("w2", Some("a"), "x86_64-linux"),
            slot("w3", Some("b"), "x86_64-linux"),
            slot("w4", Some("b"), "x86_64-linux"),
        ];

        let placement = plan(&c, &slots, &scores).expect("placed");

        let mut seated = workers(&placement);
        seated.sort();
        assert_eq!(seated, vec!["w3", "w4"]);
    }

    #[test]
    fn a_cluster_larger_than_every_zone_is_not_placed() {
        let c = cluster(true, vec![build_member("a", None), build_member("b", None)]);
        let slots = [
            slot("w1", Some("a"), "x86_64-linux"),
            slot("w2", Some("b"), "x86_64-linux"),
        ];

        assert!(plan(&c, &slots, &ScoreLookup::new()).is_none());
    }
}
