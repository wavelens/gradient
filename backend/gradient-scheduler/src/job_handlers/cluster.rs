/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs: placement, prepare, start and signal forwarding.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_db::scheduling::cluster::ClusterClaim;
use gradient_pool::session_port::SessionSignal;
use gradient_types::ids::ClusterAttemptId;
use gradient_wire::types::{ClusterAddress, ClusterMembership, ClusterPeer};
use tracing::{debug, info, warn};

/// How long a resolved attempt waits for a survivor that never reports back.
const RESOLVED_ATTEMPT_TTL: Duration = Duration::from_secs(600);

use super::assignment::{assigned_transition, assignment_row, claim_gate};
use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::cluster::{
    Acceptance, AgingPolicy, AgingStep, AttemptMember, AttemptState, CLUSTER_HOLD_MARGIN_SECS,
    CLUSTER_RETRY_BACKOFF, ClusterSnapshot, CommittedSeat, Committing, PendingCluster, Placement,
    PreparedMember, Reservation, aging_step, hide_reserved,
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
        let pending = self.attempts.lock().pending_verdicts();
        for (attempt, fate) in pending {
            if let Err(e) = self.resolve_attempt(attempt, fate).await {
                warn!(error = %e, %attempt, "resolving a cluster attempt failed; retrying");
            }
        }
        self.attempts
            .lock()
            .expire_resolved(Instant::now(), RESOLVED_ATTEMPT_TTL);
        self.abort_dead_clusters().await?;

        let mut snapshot = self.cluster_snapshot().await;
        let aging = snapshot.clone();
        hide_reserved(&mut snapshot);
        let reserved = snapshot.reservation.as_ref().map(Reservation::cluster);
        let mut slots = snapshot.slots;
        let mut seated: Vec<String> = Vec::new();
        for cluster in snapshot.clusters.iter().filter(|c| Some(c.id) != reserved) {
            let Some(placement) = crate::cluster::plan(cluster, &slots, &snapshot.scores) else {
                continue;
            };
            slots.retain(|s| !placement.seats.iter().any(|seat| seat.worker == s.worker));
            let workers: Vec<String> = placement.seats.iter().map(|s| s.worker.clone()).collect();
            if self.commit(placement).await {
                seated.extend(workers);
            }
        }
        let mut aging = aging;
        aging.slots.retain(|s| !seated.contains(&s.worker));
        self.age(&aging).await;

        Ok(())
    }

    /// A cluster that waited too long for simultaneously idle slots reserves
    /// seats and commits once all of them are idle.
    async fn age(&self, snapshot: &ClusterSnapshot) {
        let config = &self.state.config.scheduler;
        let policy = AgingPolicy {
            reserve_after: chrono::Duration::seconds(
                i64::try_from(config.cluster_reserve_after_secs).unwrap_or(i64::MAX),
            ),
            timeout: Duration::from_secs(config.cluster_reserve_timeout_secs),
        };
        match aging_step(snapshot, gradient_types::now(), Instant::now(), &policy) {
            AgingStep::Keep => {}
            AgingStep::Expire => {
                if let Some(held) = &snapshot.reservation {
                    info!(cluster = %held.cluster(), "cluster reservation released");
                    self.release_reservation(held.cluster()).await;
                }
            }
            AgingStep::Reserve(reservation) => {
                if let Some(held) = &snapshot.reservation
                    && held.cluster() != reservation.cluster()
                {
                    self.release_reservation(held.cluster()).await;
                }
                self.reserve(reservation).await;
            }
            AgingStep::Commit(placement) => {
                self.commit(placement).await;
            }
        }
    }

    /// `true` when the placement's workers were taken for the attempt.
    pub(crate) async fn commit(&self, placement: Placement) -> bool {
        let attempt = ClusterAttemptId::now_v7();
        let Some(Committing { cluster, seats }) = self.take_placement(placement, attempt).await
        else {
            return false;
        };
        let cluster_id = cluster.id;
        // Owned by the book from here on: a pass dropped mid-commit still
        // leaves the attempt to its deadline instead of stranding the cluster.
        self.open_attempt(attempt, cluster, &seats);

        let now = gradient_types::now();
        let claim = ClusterClaim {
            cluster: cluster_id,
            attempt,
            now,
            members: seats
                .iter()
                .map(|s| {
                    (
                        assignment_row(&s.record, &s.worker, now),
                        claim_gate(&s.record),
                    )
                })
                .collect(),
        };

        match gradient_db::scheduling::cluster::claim_cluster(&self.state.worker_db, claim).await {
            Ok(true) => self.prepare(attempt, seats).await,
            Ok(false) => self.hand_back(attempt).await,
            Err(e) => {
                warn!(error = %e, %attempt, "cluster claim failed");
                self.back_off(attempt).await;
            }
        }

        true
    }

    fn open_attempt(
        &self,
        attempt: ClusterAttemptId,
        cluster: PendingCluster,
        seats: &[CommittedSeat],
    ) {
        let timeout = self.state.config.scheduler.cluster_prepare_timeout_secs;
        let roster = seats
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
                report: None,
                settled: false,
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
                all_accepted: false,
                verdict: None,
                resolving: false,
                resolution: None,
                resolved_at: None,
            },
        );
    }

    /// A lost claim: another instance holds the cluster or a member's gate no
    /// longer holds. The members go back to the startable feed, which re-reads them.
    async fn hand_back(&self, attempt: ClusterAttemptId) {
        let Some(state) = self.attempts.lock().take(attempt) else {
            return;
        };
        let seats = state
            .members
            .iter()
            .map(|m| (m.worker.clone(), m.job_id.clone()))
            .collect();
        if let Err(e) = self
            .call(|reply| SchedulerMsg::DropCluster { seats, reply })
            .await
        {
            warn!(error = %e, %attempt, "cluster hand-back did not reach the scheduler");
        }
    }

    async fn back_off(&self, attempt: ClusterAttemptId) {
        let Some(state) = self.attempts.lock().take(attempt) else {
            return;
        };
        self.restore_parked(attempt, state).await;
    }

    async fn restore_parked(&self, attempt: ClusterAttemptId, state: AttemptState) {
        let seats = state
            .members
            .iter()
            .map(|m| (m.worker.clone(), m.job_id.clone()))
            .collect();
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

    async fn prepare(&self, attempt: ClusterAttemptId, seats: Vec<CommittedSeat>) {
        let timeout = self.state.config.scheduler.cluster_prepare_timeout_secs;
        let hold_secs = u32::try_from(timeout)
            .unwrap_or(u32::MAX)
            .saturating_add(CLUSTER_HOLD_MARGIN_SECS);
        for seat in &seats {
            if let Err(e) = assigned_transition(&self.state, &seat.record).await {
                warn!(error = %e, %attempt, "cluster member transition failed");
                self.fail_prepare(attempt).await;
                return;
            }
        }

        let mut signals = Vec::with_capacity(seats.len());
        for seat in seats {
            self.announce_assignment(&seat.worker, &seat.record);
            let membership = ClusterMembership {
                attempt: attempt.to_string(),
                role: seat.role,
                index: seat.index,
                hold_secs,
            };
            let assignment = Assignment {
                job: seat.job.clone().into_job(),
                project_id: seat.job.project_id(),
                assignment_record: seat.record,
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
        match gradient_db::scheduling::cluster::start_cluster_attempt(
            &self.state.worker_db,
            cluster,
            attempt,
        )
        .await
        {
            Ok(true) => {
                let Some(decided) = self.attempts.lock().mark_started(attempt) else {
                    return;
                };
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
                if let Some(fate) = decided
                    && let Err(e) = self.resolve_attempt(attempt, fate).await
                {
                    warn!(error = %e, %attempt, "resolving a cluster attempt failed; retrying");
                }
            }
            Ok(false) => self.fail_prepare(attempt).await,
            Err(e) => {
                warn!(error = %e, %attempt, "cluster start failed");
                self.fail_prepare(attempt).await;
            }
        }
    }

    /// A member that ends before its attempt started never ran: it fails the
    /// prepare instead of reaching the graph. `true` when that happened.
    pub async fn cluster_member_released(&self, job_id: &str) -> bool {
        let Some(attempt) = self.attempts.lock().preparing(job_id) else {
            return false;
        };
        if self.is_aborting(job_id).await {
            return false;
        }
        self.fail_prepare(attempt).await;

        true
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
        if let Err(e) = gradient_db::scheduling::cluster::fail_prepare_attempt(
            &self.state.worker_db,
            state.cluster,
            attempt,
        )
        .await
        {
            // An open attempt blocks every later claim of the cluster: keep it
            // owned, overdue, so the next pass retries the close.
            warn!(error = %e, %attempt, "prepare-failed attempt left open; retrying");
            let mut state = state;
            state.started = false;
            state.all_accepted = false;
            self.attempts.lock().open(attempt, state);
            return;
        }

        let mut signals = Vec::with_capacity(state.members.len());
        {
            let mut prepared = self.prepared.lock();
            for m in &state.members {
                prepared.remove(&m.job_id);
                let abort = SessionSignal::AbortCluster {
                    attempt: attempt.to_string(),
                    reason: "cluster prepare failed".into(),
                };
                signals.push((m.worker.clone(), abort));
            }
        }
        self.signal_workers(signals).await;
        self.restore_parked(attempt, state).await;
    }

    pub async fn forward_cluster_signal(
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
        if let Err(e) = self
            .call(|reply| SchedulerMsg::SignalWorkers { signals, reply })
            .await
        {
            warn!(error = %e, "cluster signals did not reach the scheduler");
        }
    }

    pub async fn reserve(&self, reservation: crate::cluster::Reservation) -> bool {
        self.call(|reply| SchedulerMsg::Reserve { reservation, reply })
            .await
            .unwrap_or(false)
    }

    pub async fn release_reservation(&self, cluster: gradient_types::ids::ClusterJobId) {
        let _ = self
            .cast(SchedulerMsg::ReleaseReservation { cluster })
            .await;
    }

    pub async fn reservation(&self) -> Option<crate::cluster::Reservation> {
        self.call(|reply| SchedulerMsg::Reservation { reply })
            .await
            .ok()
            .flatten()
    }

    /// A `Queued` cluster with a member that can no longer run is aborted; its
    /// waiting members settle as aborted so their builds and evaluations finish.
    async fn abort_dead_clusters(&self) -> anyhow::Result<()> {
        for cluster in
            gradient_db::scheduling::cluster::abort_dead_queued_clusters(&self.state.worker_db)
                .await?
        {
            let dropped = self
                .call(|reply| SchedulerMsg::DropWaiting { cluster, reply })
                .await?;
            let Some(waiting) = dropped else {
                continue;
            };
            let failure = crate::cluster::Failure {
                error: "a member of its cluster job can no longer run".into(),
                kind: gradient_wire::types::BuildFailureKind::Aborted,
                missing_paths: Vec::new(),
            };
            for job in waiting.members.into_iter().filter_map(|m| m.job) {
                if let Err(e) = self.settle_failed(job, &failure).await {
                    warn!(error = %e, %cluster, "settling a member of an aborted cluster failed");
                }
            }
        }

        Ok(())
    }

    async fn is_aborting(&self, job_id: &str) -> bool {
        let job_id = job_id.to_owned();
        self.call(|reply| SchedulerMsg::IsAborting { job_id, reply })
            .await
            .unwrap_or(false)
    }
}
