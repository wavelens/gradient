/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs: placement, prepare, start and signal relay.

use gradient_types::ids::ClusterAttemptId;
use tracing::warn;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::cluster::{Committing, Placement};

impl Scheduler {
    pub async fn take_placement(
        &self,
        placement: Placement,
        attempt: ClusterAttemptId,
    ) -> Option<Committing> {
        let instance = self.instance.load_full();
        self.call(|reply| SchedulerMsg::TakePlacement {
            placement,
            attempt,
            instance,
            reply,
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn restore_cluster(&self, committing: Committing) {
        let seats = committing
            .seats
            .iter()
            .map(|s| (s.worker.clone(), s.key.clone()))
            .collect();
        let cluster = committing.cluster;
        if let Err(e) = self
            .call(|reply| SchedulerMsg::RestoreCluster {
                cluster,
                seats,
                reply,
            })
            .await
        {
            warn!(error = %e, "cluster restore did not reach the scheduler");
        }
    }
}
