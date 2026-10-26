/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::cmp::Reverse;

use gradient_db::scheduling::cluster::{dissolve_split_cluster, drop_split_companions};
use gradient_types::ids::{ClusterJobId, ClusterMemberId};
use tracing::{info, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::cluster::{PendingCluster, Placement, ScoreLookup, Slot, fillable_seats, plan};
use crate::jobs::parse_eval_job_key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shrink {
    Drop(Vec<ClusterMemberId>),
    Dissolve,
}

pub fn shrink(cluster: &PendingCluster, fillable: usize) -> Option<Shrink> {
    let split = cluster.members.iter().any(|m| m.split);
    if !split || fillable >= cluster.members.len() {
        return None;
    }

    if fillable < 2 {
        return Some(Shrink::Dissolve);
    }

    let mut companions: Vec<(u16, ClusterMemberId)> = cluster
        .members
        .iter()
        .filter(|m| !m.primary)
        .map(|m| {
            (
                parse_eval_job_key(&m.key).map_or(0, |(_, index)| index),
                m.id,
            )
        })
        .collect();
    companions.sort_by_key(|&(index, _)| Reverse(index));
    let surplus = cluster.members.len() - fillable;

    Some(Shrink::Drop(
        companions
            .into_iter()
            .take(surplus)
            .map(|(_, id)| id)
            .collect(),
    ))
}

impl Scheduler {
    pub(crate) async fn shrink_split(
        &self,
        cluster: &PendingCluster,
        slots: &[Slot],
        scores: &ScoreLookup,
    ) -> Option<Placement> {
        match shrink(cluster, fillable_seats(cluster, slots, scores))? {
            Shrink::Dissolve => {
                self.dissolve_split(cluster.id).await;
                None
            }
            Shrink::Drop(members) => {
                let shrunk = self.drop_companions(cluster.id, members).await?;
                plan(&shrunk, slots, scores)
            }
        }
    }

    async fn dissolve_split(&self, cluster: ClusterJobId) {
        match dissolve_split_cluster(&self.db.worker_db, cluster).await {
            Ok(true) => self.forget_dissolved(vec![cluster]).await,
            Ok(false) => {}
            Err(e) => {
                warn!(%cluster, error = %e, "dissolving a split cluster without seats failed")
            }
        }
    }

    async fn drop_companions(
        &self,
        cluster: ClusterJobId,
        members: Vec<ClusterMemberId>,
    ) -> Option<PendingCluster> {
        if let Err(e) = drop_split_companions(&self.db.worker_db, cluster, &members).await {
            warn!(%cluster, error = %e, "dropping split companions without seats failed");
            return None;
        }

        info!(%cluster, dropped = members.len(), "split cluster shrunk to its free seats");
        self.call(|reply| SchedulerMsg::DropWaitingMembers {
            cluster,
            members,
            reply,
        })
        .await
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ClusterMember;
    use crate::jobs::eval_job_key;
    use gradient_types::ids::EvaluationId;

    fn split_cluster(members: u16) -> PendingCluster {
        let evaluation = EvaluationId::now_v7();
        PendingCluster {
            id: ClusterJobId::now_v7(),
            same_zone: false,
            queued_at: gradient_types::now(),
            startable_since: None,
            expected: usize::from(members),
            members: (0..members)
                .map(|index| ClusterMember {
                    id: ClusterMemberId::now_v7(),
                    role: "split".into(),
                    primary: index == 0,
                    pin: None,
                    key: eval_job_key(evaluation, index),
                    job: None,
                    split: true,
                    min_free_ram_mb: None,
                })
                .collect(),
            not_before: None,
        }
    }

    #[test]
    fn the_companions_with_the_highest_index_go_first() {
        let cluster = split_cluster(4);
        let last_two = vec![cluster.members[3].id, cluster.members[2].id];

        assert_eq!(shrink(&cluster, 2), Some(Shrink::Drop(last_two)));
        assert_eq!(shrink(&cluster, 1), Some(Shrink::Dissolve));
        assert_eq!(shrink(&cluster, 4), None);
    }
}
