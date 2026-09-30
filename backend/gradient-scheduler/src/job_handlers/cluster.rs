/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs: placement, prepare, start and signal relay.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_db::ClusterClaim;
use gradient_entity::cluster_attempt::ClusterAttemptOutcome;
use gradient_pool::session_port::SessionSignal;
use gradient_types::ids::ClusterAttemptId;
use gradient_wire::types::{ClusterAddress, ClusterMembership, ClusterPeer};
use tracing::{debug, warn};

use super::assignment::{claim_gate, dispatch_row, dispatched_transition};
use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::cluster::{
    Acceptance, AttemptMember, AttemptState, CLUSTER_HOLD_MARGIN_SECS, CLUSTER_RETRY_BACKOFF,
    Committing, Placement, PreparedMember,
};
use crate::jobs::Assignment;

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

    /// The cluster-dispatch pass: expire overdue prepares, then place every
    /// ready cluster, oldest first, on slots no earlier cluster of this pass took.
    pub async fn plan_clusters(self: &Arc<Self>) -> anyhow::Result<()> {
        let overdue = self.attempts.lock().overdue(Instant::now());
        for attempt in overdue {
            self.fail_prepare(attempt).await;
        }

        let snapshot = self.cluster_snapshot().await;
        let mut slots = snapshot.slots;
        for cluster in &snapshot.clusters {
            let Some(placement) = crate::cluster::plan(cluster, &slots, &snapshot.scores) else {
                continue;
            };
            slots.retain(|s| !placement.seats.iter().any(|seat| seat.worker == s.worker));
            self.commit(placement).await;
        }

        Ok(())
    }

    pub(crate) async fn commit(&self, placement: Placement) {
        let attempt = ClusterAttemptId::now_v7();
        let Some(committing) = self.take_placement(placement, attempt).await else {
            return;
        };
        let now = gradient_types::now();
        let claim = ClusterClaim {
            cluster: committing.cluster.id,
            attempt,
            now,
            members: committing
                .seats
                .iter()
                .map(|s| {
                    (
                        dispatch_row(&s.record, &s.worker, now),
                        claim_gate(&s.record),
                    )
                })
                .collect(),
        };

        match gradient_db::claim_cluster(&self.state.worker_db, claim).await {
            Ok(true) => self.prepare(attempt, committing).await,
            Ok(false) => self.back_off(committing).await,
            Err(e) => {
                warn!(error = %e, cluster = %committing.cluster.id, "cluster claim failed");
                self.back_off(committing).await;
            }
        }
    }

    async fn back_off(&self, mut committing: Committing) {
        committing.cluster.not_before = Some(Instant::now() + CLUSTER_RETRY_BACKOFF);
        self.restore_cluster(committing).await;
    }

    async fn prepare(&self, attempt: ClusterAttemptId, committing: Committing) {
        let Committing { cluster, seats } = committing;
        let timeout = self.state.config.scheduler.cluster_prepare_timeout_secs;
        let hold_secs = u32::try_from(timeout)
            .unwrap_or(u32::MAX)
            .saturating_add(CLUSTER_HOLD_MARGIN_SECS);
        let roster: Vec<ClusterPeer> = seats
            .iter()
            .map(|s| ClusterPeer {
                role: s.role.clone(),
                index: s.index,
                worker: s.worker.clone(),
                zone: s.zone.clone(),
                endpoint: s.endpoint.clone(),
            })
            .collect();
        let members = seats
            .iter()
            .map(|s| AttemptMember {
                job_id: s.key.clone(),
                worker: s.worker.clone(),
                role: s.role.clone(),
                index: s.index,
                primary: s.primary,
                accepted: false,
                outcome: None,
            })
            .collect();
        self.attempts.lock().open(
            attempt,
            AttemptState {
                cluster: cluster.id,
                parked: cluster,
                members,
                roster,
                deadline: Instant::now() + Duration::from_secs(timeout),
                started: false,
            },
        );

        for seat in &seats {
            if let Err(e) = dispatched_transition(&self.state, &seat.record).await {
                warn!(error = %e, %attempt, "cluster member transition failed");
                self.fail_prepare(attempt).await;
                return;
            }
        }

        let mut signals = Vec::with_capacity(seats.len());
        for seat in seats {
            self.announce_dispatch(&seat.worker, &seat.record);
            let membership = ClusterMembership {
                attempt: attempt.to_string(),
                role: seat.role,
                index: seat.index,
                hold_secs,
            };
            let assignment = Assignment {
                job: seat.job.clone().into_job(),
                project_id: seat.job.project_id(),
                dispatch_record: seat.record,
                pending: seat.job,
            };
            self.prepared.lock().insert(
                seat.key.clone(),
                PreparedMember {
                    assignment,
                    membership,
                },
            );
            signals.push((
                seat.worker,
                SessionSignal::ClusterAssign { job_id: seat.key },
            ));
        }
        self.signal_workers(signals).await;
    }

    pub fn take_prepared(&self, job_id: &str) -> Option<PreparedMember> {
        self.prepared.lock().remove(job_id)
    }

    pub async fn cluster_member_accepted(&self, job_id: &str) {
        let Acceptance::AllAccepted {
            cluster,
            attempt,
            workers,
            roster,
        } = self.attempts.lock().accept(job_id)
        else {
            return;
        };
        match gradient_db::start_cluster_attempt(&self.state.worker_db, cluster, attempt).await {
            Ok(true) => {
                self.attempts.lock().mark_started(attempt);
                let signals = workers
                    .into_iter()
                    .map(|w| {
                        let start = SessionSignal::StartCluster {
                            attempt: attempt.to_string(),
                            roster: roster.clone(),
                        };
                        (w, start)
                    })
                    .collect();
                self.signal_workers(signals).await;
            }
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, %attempt, "cluster start failed");
                self.fail_prepare(attempt).await;
            }
        }
    }

    /// `true` when `job_id` was a member of an open attempt, which failed with it.
    pub async fn cluster_member_rejected(&self, job_id: &str) -> bool {
        let Some(attempt) = self.attempts.lock().attempt_of(job_id) else {
            return false;
        };
        self.fail_prepare(attempt).await;

        true
    }

    pub async fn fail_prepare(&self, attempt: ClusterAttemptId) {
        let Some(state) = self.attempts.lock().take(attempt) else {
            return;
        };
        if let Err(e) = gradient_db::close_cluster_attempt(
            &self.state.worker_db,
            attempt,
            ClusterAttemptOutcome::PrepareFailed,
        )
        .await
        {
            warn!(error = %e, %attempt, "prepare-failed attempt left open");
        }

        let mut signals = Vec::with_capacity(state.members.len());
        let mut seats = Vec::with_capacity(state.members.len());
        {
            let mut prepared = self.prepared.lock();
            for m in &state.members {
                prepared.remove(&m.job_id);
                seats.push((m.worker.clone(), m.job_id.clone()));
                let abort = SessionSignal::AbortCluster {
                    attempt: attempt.to_string(),
                    reason: "cluster prepare failed".into(),
                };
                signals.push((m.worker.clone(), abort));
            }
        }
        self.signal_workers(signals).await;

        let mut cluster = state.parked;
        cluster.not_before = Some(Instant::now() + CLUSTER_RETRY_BACKOFF);
        if let Err(e) = self
            .call(|reply| SchedulerMsg::RestoreCluster {
                cluster,
                seats,
                reply,
            })
            .await
        {
            warn!(error = %e, %attempt, "cluster restore did not reach the scheduler");
        }
    }

    pub async fn relay_cluster_signal(
        &self,
        from_worker: &str,
        attempt: &str,
        to: Option<ClusterAddress>,
        payload: Vec<u8>,
    ) {
        let Ok(attempt_id) = attempt.parse::<ClusterAttemptId>() else {
            return;
        };
        let Some(route) = self
            .attempts
            .lock()
            .route(attempt_id, from_worker, to.as_ref())
        else {
            debug!(%from_worker, %attempt, "cluster signal dropped");
            return;
        };
        let signals = route
            .workers
            .into_iter()
            .map(|w| {
                let signal = SessionSignal::ClusterSignal {
                    attempt: attempt.to_owned(),
                    from: route.from.clone(),
                    payload: payload.clone(),
                };
                (w, signal)
            })
            .collect();
        self.signal_workers(signals).await;
    }

    pub(crate) async fn signal_workers(&self, signals: Vec<(String, SessionSignal)>) {
        if let Err(e) = self.cast(SchedulerMsg::SignalWorkers { signals }).await {
            warn!(error = %e, "cluster signals did not reach the scheduler");
        }
    }
}
