/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::time::Instant;

use chrono::NaiveDateTime;
use gradient_db::scheduling::cluster::MemberOf;
use gradient_types::ids::{ClusterJobId, ClusterMemberId};

use super::SlotKind;
use crate::jobs::PendingJob;

#[derive(Debug, Clone)]
pub struct ClusterMember {
    pub id: ClusterMemberId,
    pub role: String,
    pub primary: bool,
    pub pin: Option<String>,
    pub key: String,
    pub job: Option<PendingJob>,
}

#[derive(Debug, Clone)]
pub struct PendingCluster {
    pub id: ClusterJobId,
    pub same_zone: bool,
    pub queued_at: NaiveDateTime,
    pub expected: usize,
    pub members: Vec<ClusterMember>,
    pub not_before: Option<Instant>,
}

impl PendingCluster {
    fn new(of: &MemberOf) -> Self {
        Self {
            id: of.cluster.id,
            same_zone: of.cluster.same_zone,
            queued_at: of.cluster.created_at,
            expected: of.member_count as usize,
            members: Vec::new(),
            not_before: None,
        }
    }

    pub fn ready(&self) -> bool {
        self.members.len() == self.expected && self.members.iter().all(|m| m.job.is_some())
    }

    pub fn prioritized(&self) -> bool {
        self.members
            .iter()
            .any(|m| m.job.as_ref().is_some_and(PendingJob::prioritized))
    }

    pub fn slot_kind(m: &ClusterMember) -> SlotKind {
        if m.key
            .starts_with(gradient_db::scheduling::assignment_record::BUILD_KEY_PREFIX)
        {
            SlotKind::Build
        } else {
            SlotKind::Eval
        }
    }
}

/// A member key in the book is counting as tracked. The dispatch passes must never enqueue it
/// again.
#[derive(Debug, Default)]
pub struct ClusterBook {
    waiting: HashMap<ClusterJobId, PendingCluster>,
    by_key: HashMap<String, ClusterJobId>,
}

impl ClusterBook {
    pub fn add(&mut self, of: MemberOf, key: String, job: PendingJob) {
        if self.claiming(&key) {
            return;
        }
        let cluster = self
            .waiting
            .entry(of.cluster.id)
            .or_insert_with(|| PendingCluster::new(&of));
        cluster.expected = of.member_count as usize;
        match cluster.members.iter_mut().find(|m| m.id == of.member.id) {
            Some(member) => member.job = Some(job),
            None => cluster.members.push(ClusterMember {
                id: of.member.id,
                role: of.member.role,
                primary: of.member.primary,
                pin: of.member.pin,
                key: key.clone(),
                job: Some(job),
            }),
        }
        self.by_key.insert(key, of.cluster.id);
    }

    pub fn get(&self, id: ClusterJobId) -> Option<&PendingCluster> {
        self.waiting.get(&id)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.by_key.contains_key(key)
    }

    pub fn ready(&self) -> impl Iterator<Item = &PendingCluster> {
        self.waiting.values().filter(|c| c.ready())
    }

    /// A taken cluster's keys stay tracked until it is restored or its members are released into
    /// active jobs. No pass can enqueue them meanwhile.
    pub fn take(&mut self, id: ClusterJobId) -> Option<PendingCluster> {
        self.waiting.remove(&id)
    }

    pub fn drop_cluster(&mut self, id: ClusterJobId) -> Option<PendingCluster> {
        let cluster = self.waiting.remove(&id)?;
        for member in &cluster.members {
            self.by_key.remove(&member.key);
        }
        Some(cluster)
    }

    pub fn release(&mut self, key: &str) {
        self.by_key.remove(key);
    }

    fn claiming(&self, key: &str) -> bool {
        self.by_key
            .get(key)
            .is_some_and(|id| !self.waiting.contains_key(id))
    }

    pub fn restore(&mut self, cluster: PendingCluster) {
        for member in cluster.members.iter().filter(|m| m.job.is_some()) {
            self.by_key.insert(member.key.clone(), cluster.id);
        }
        self.waiting.insert(cluster.id, cluster);
    }

    pub fn forget(&mut self, key: &str) -> bool {
        let Some(id) = self.by_key.remove(key) else {
            return false;
        };
        if let Some(member) = self
            .waiting
            .get_mut(&id)
            .and_then(|c| c.members.iter_mut().find(|m| m.key == key))
        {
            member.job = None;
        }
        true
    }

    pub fn jobs_mut(&mut self) -> impl Iterator<Item = &mut PendingJob> {
        self.waiting
            .values_mut()
            .flat_map(|c| c.members.iter_mut())
            .filter_map(|m| m.job.as_mut())
    }

    pub fn jobs(&self) -> impl Iterator<Item = (&String, &PendingJob)> {
        self.waiting
            .values()
            .flat_map(|c| c.members.iter())
            .filter_map(|m| m.job.as_ref().map(|j| (&m.key, j)))
    }
}

#[cfg(test)]
pub(crate) mod book_tests {
    use super::*;
    use gradient_entity::cluster_job::Model as MClusterJob;
    use gradient_entity::cluster_member::Model as MClusterMember;
    use gradient_types::ids::ProjectId;

    pub(crate) fn member_of(cluster: ClusterJobId, count: u32) -> MemberOf {
        MemberOf {
            cluster: MClusterJob {
                id: cluster,
                ..Default::default()
            },
            member: MClusterMember {
                id: ClusterMemberId::now_v7(),
                cluster_job: cluster,
                role: "node".into(),
                ..Default::default()
            },
            member_count: count,
        }
    }

    fn eval() -> PendingJob {
        crate::jobs::test_eval_job(ProjectId::now_v7())
    }

    #[test]
    fn a_cluster_is_ready_only_when_every_member_arrived() {
        let id = ClusterJobId::now_v7();
        let mut book = ClusterBook::default();

        book.add(member_of(id, 2), "eval:a".into(), eval());
        assert_eq!(book.ready().count(), 0);
        assert!(book.contains("eval:a"));

        book.add(member_of(id, 2), "eval:b".into(), eval());
        assert_eq!(book.ready().count(), 1);
    }

    #[test]
    fn a_forgotten_member_unreadies_its_cluster_until_it_returns() {
        let id = ClusterJobId::now_v7();
        let first = member_of(id, 2);
        let mut book = ClusterBook::default();
        book.add(first.clone(), "eval:a".into(), eval());
        book.add(member_of(id, 2), "eval:b".into(), eval());

        assert!(book.forget("eval:a"));
        assert_eq!(book.ready().count(), 0);
        assert!(!book.contains("eval:a"));

        book.add(first, "eval:a".into(), eval());
        assert_eq!(book.ready().count(), 1);
        assert_eq!(book.ready().next().expect("ready").members.len(), 2);
    }

    #[test]
    fn a_taken_cluster_stays_tracked_but_unready_until_restored() {
        let id = ClusterJobId::now_v7();
        let mut book = ClusterBook::default();
        let member = member_of(id, 1);
        book.add(member.clone(), "build:x".into(), eval());

        let taken = book.take(id).expect("taken");
        assert!(book.contains("build:x"));
        assert_eq!(book.ready().count(), 0);
        book.add(member, "build:x".into(), eval());
        assert_eq!(
            book.ready().count(),
            0,
            "a claimed member is not added twice"
        );

        book.restore(taken);
        assert!(book.contains("build:x"));
        assert_eq!(book.ready().count(), 1);
    }
}
