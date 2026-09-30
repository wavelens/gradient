/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The scheduler's state as one actor: `WorkerPool` and `JobTracker` are its
//! private state, every mutation is a message, and sessions are reached only
//! through a [`SessionPort`].

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::{Duration, Instant};

use gradient_pool::score::{InstanceContext, ScoringPolicy};
use gradient_types::ids::{
    ClusterAttemptId, DerivationBuildId, DispatchedJobId, EvaluationId, ProjectId,
};
use gradient_wire::types::{CandidateScore, GradientCapabilities, JobCandidate, JobKind};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use tracing::{debug, info};

use crate::jobs::{
    Assignment, BoardActiveJob, CandidateDetail, DispatchDecision, JobTracker, PendingBuildJob,
    PendingJob, PendingJobInfo,
};
use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_pool::{WorkerCaps, WorkerInfo, WorkerPool};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Registration {
    pub worker: String,
    pub capabilities: GradientCapabilities,
    pub authorized_peers: HashSet<ProjectId>,
    pub session: Arc<dyn SessionPort>,
    pub active: Vec<crate::jobs::Reattached>,
}

pub struct Registered {
    pub last_seen: Arc<AtomicI64>,
}

pub type WorkerCapabilities = gradient_pool::WorkerProfile;

#[derive(Debug, Clone, Copy)]
pub struct WorkerMetrics {
    pub cpu_usage_pct: f32,
    pub ram_free_mb: u64,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub candidates: Vec<JobCandidate>,
    pub generation: u64,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one assignment per RequestJob reply; boxing only relocates the allocation"
)]
pub enum AssignOutcome {
    AtCapacity,
    Nothing,
    Assigned(Assignment),
}

pub struct Released {
    pub job: Option<PendingJob>,
    pub worker_idle: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub workers: usize,
    pub idle_workers: usize,
    pub pending: usize,
    pub active: usize,
    pub pending_builds: u32,
    pub active_builds: u32,
    pub cpu_core_score_mean: Option<f64>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one message per mailbox send; boxing only relocates the allocation"
)]
pub enum SchedulerMsg {
    Register(Registration, RpcReplyPort<Registered>),
    Unregister {
        worker: String,
        reply: RpcReplyPort<crate::jobs::Disconnected>,
    },
    IsConnected {
        worker: String,
        reply: RpcReplyPort<bool>,
    },
    AuthorizedFor {
        worker: String,
        project: ProjectId,
        reply: RpcReplyPort<bool>,
    },
    UpdatePeers {
        worker: String,
        peers: HashSet<ProjectId>,
        reply: RpcReplyPort<()>,
    },
    RevokePeers {
        worker: String,
        revoked: HashSet<ProjectId>,
        reply: RpcReplyPort<usize>,
    },
    Reauth {
        worker: String,
    },
    UpdateCapabilities {
        worker: String,
        caps: WorkerCapabilities,
        reply: RpcReplyPort<()>,
    },
    UpdateMetrics {
        worker: String,
        metrics: WorkerMetrics,
        reply: RpcReplyPort<()>,
    },
    MarkDraining {
        worker: String,
        reply: RpcReplyPort<()>,
    },
    Enqueue {
        job_id: String,
        job: PendingJob,
        reply: RpcReplyPort<()>,
    },
    EnqueueMember {
        of: gradient_db::MemberOf,
        key: String,
        job: PendingJob,
        reply: RpcReplyPort<()>,
    },
    ClusterSnapshot {
        reply: RpcReplyPort<crate::cluster::ClusterSnapshot>,
    },
    TakePlacement {
        placement: crate::cluster::Placement,
        attempt: ClusterAttemptId,
        instance: Arc<InstanceContext>,
        reply: RpcReplyPort<Option<crate::cluster::Committing>>,
    },
    RestoreCluster {
        cluster: crate::cluster::PendingCluster,
        seats: Vec<(String, String)>,
        reply: RpcReplyPort<()>,
    },
    SignalWorkers {
        signals: Vec<(String, SessionSignal)>,
    },
    Candidates {
        worker: String,
        only_new: bool,
        reply: RpcReplyPort<Offer>,
    },
    Assign {
        worker: String,
        kind: JobKind,
        instance: Arc<InstanceContext>,
        reply: RpcReplyPort<AssignOutcome>,
    },
    RecordScores {
        worker: String,
        scores: Vec<CandidateScore>,
        reply: RpcReplyPort<()>,
    },
    Rejected {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<()>,
    },
    Release {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<Released>,
    },
    AbortJob {
        worker: String,
        job_id: String,
        reason: String,
        reply: RpcReplyPort<bool>,
    },
    ReapOverdueAborts {
        grace: Duration,
        reply: RpcReplyPort<Vec<String>>,
    },
    AbortEvaluation {
        evaluation_id: EvaluationId,
        /// The anchors the database abort actually moved. A build anchor is
        /// global, so the ones it spared are still wanted by a live evaluation
        /// and their workers must keep building.
        aborted_anchors: Vec<DerivationBuildId>,
        reply: RpcReplyPort<Vec<(String, String)>>,
    },
    Prioritize {
        evaluation: Option<EvaluationId>,
        anchors: Vec<DerivationBuildId>,
        reply: RpcReplyPort<()>,
    },
    RemoveJobs {
        job_ids: Vec<String>,
        reply: RpcReplyPort<()>,
    },
    ActiveJob {
        job_id: String,
        reply: RpcReplyPort<Option<PendingJob>>,
    },
    PendingJob {
        job_id: String,
        reply: RpcReplyPort<Option<PendingJob>>,
    },
    Untracked {
        job_ids: Vec<String>,
        reply: RpcReplyPort<Vec<String>>,
    },
    PrunePendingBuilds {
        stale: Box<dyn Fn(&PendingBuildJob) -> bool + Send>,
        reply: RpcReplyPort<usize>,
    },
    HasIdleEvalOnlyWorker {
        reply: RpcReplyPort<bool>,
    },
    Workers {
        reply: RpcReplyPort<Vec<WorkerInfo>>,
    },
    WorkerCaps {
        worker: String,
        reply: RpcReplyPort<Option<GradientCapabilities>>,
    },
    StaleWorkers {
        now_ms: i64,
        timeout_ms: i64,
        reply: RpcReplyPort<Vec<String>>,
    },
    Counts {
        reply: RpcReplyPort<Counts>,
    },
    PendingSnapshot {
        reply: RpcReplyPort<Vec<PendingJobInfo>>,
    },
    BoardActiveJobs {
        reply: RpcReplyPort<Vec<BoardActiveJob>>,
    },
    RecentDecisions {
        reply: RpcReplyPort<Vec<DispatchDecision>>,
    },
    CandidateDetail {
        id: DispatchedJobId,
        reply: RpcReplyPort<Option<CandidateDetail>>,
    },
    BumpRescore {
        reply: RpcReplyPort<()>,
    },
    ReOffer,
}

pub struct CoreArgs {
    pub policy: Arc<dyn ScoringPolicy>,
}

/// Whether a worker with `caps` can run a job of `kind` at all; an idle slot of
/// a kind it cannot run is no capacity.
fn runs(caps: &WorkerCaps, kind: crate::cluster::SlotKind) -> bool {
    match kind {
        crate::cluster::SlotKind::Eval => caps.capabilities.eval || caps.fetch,
        crate::cluster::SlotKind::Build => caps.capabilities.build,
    }
}

pub struct SchedulerCore {
    pool: WorkerPool,
    tracker: JobTracker,
    offers: u64,
    policy: Arc<dyn ScoringPolicy>,
    idle: crate::cluster::IdleSlots,
}

impl SchedulerCore {
    fn bump_offers(&mut self) {
        self.offers = self.offers.wrapping_add(1);
        self.pool.signal_active(SessionSignal::Offers(self.offers));
    }

    fn auth_and_caps(&self, worker: &str) -> (Option<HashSet<ProjectId>>, Option<WorkerCaps>) {
        let authorized = self
            .pool
            .peer_auth_for(worker)
            .and_then(|a| a.as_filter())
            .cloned();
        (authorized, self.pool.worker_caps(worker))
    }

    fn candidates(&mut self, worker: &str, only_new: bool) -> Offer {
        let (authorized, caps) = self.auth_and_caps(worker);
        let mut candidates = self
            .tracker
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        let visible: HashSet<String> = candidates.iter().map(|c| c.job_id.clone()).collect();
        if only_new && let Some(sent) = self.pool.sent_candidates_for(worker) {
            candidates.retain(|c| !sent.contains(&c.job_id));
        }
        self.pool.set_sent_candidates(worker, visible);
        Offer {
            candidates,
            generation: self.offers,
        }
    }

    fn assign(
        &mut self,
        worker: &str,
        kind: &JobKind,
        instance: &InstanceContext,
    ) -> AssignOutcome {
        let slot = crate::cluster::SlotKind::from(kind);
        if !self.pool.has_capacity(worker, kind) {
            debug!(%worker, ?kind, "RequestJob ignored - worker at capacity");
            self.idle.clear(worker, slot);
            return AssignOutcome::AtCapacity;
        }
        let (authorized, caps) = self.auth_and_caps(worker);
        let policy = Arc::clone(&self.policy);
        match self.tracker.take_best_of_kind(
            worker,
            authorized.as_ref(),
            caps.as_ref(),
            kind,
            &*policy,
            instance,
        ) {
            Some(assignment) => {
                self.idle.clear(worker, slot);
                self.pool.assign_job(worker, assignment.job_id());
                AssignOutcome::Assigned(assignment)
            }
            None => {
                if caps.as_ref().is_some_and(|c| runs(c, slot)) {
                    self.idle.record(worker, slot, Instant::now());
                }
                AssignOutcome::Nothing
            }
        }
    }

    fn take_placement(
        &mut self,
        placement: &crate::cluster::Placement,
        attempt: ClusterAttemptId,
        instance: &InstanceContext,
    ) -> Option<crate::cluster::Committing> {
        use crate::cluster::{CommittedSeat, PendingCluster};

        let now = Instant::now();
        let seats_free = {
            let cluster = self
                .tracker
                .waiting_cluster(placement.cluster)
                .filter(|c| c.ready())?;
            placement.seats.iter().all(|s| {
                let kind = PendingCluster::slot_kind(&cluster.members[s.member]);
                self.idle.is_idle(&s.worker, kind, now)
                    && self.pool.has_capacity(&s.worker, &kind.job_kind())
            })
        };
        if !seats_free {
            return None;
        }

        let cluster = self.tracker.take_cluster(placement.cluster)?;
        let mut roles: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        let index_of: Vec<u32> = cluster
            .members
            .iter()
            .map(|m| {
                let next = roles.entry(m.role.as_str()).or_default();
                *next += 1;
                *next - 1
            })
            .collect();

        let policy = Arc::clone(&self.policy);
        let seats: Vec<CommittedSeat> = placement
            .seats
            .iter()
            .filter_map(|s| {
                let m = &cluster.members[s.member];
                let job = m.job.clone()?;
                let caps = self.pool.worker_caps(&s.worker);
                self.idle.clear(&s.worker, PendingCluster::slot_kind(m));
                Some(CommittedSeat {
                    worker: s.worker.clone(),
                    key: m.key.clone(),
                    record: self.tracker.member_record(
                        &s.worker,
                        caps.as_ref(),
                        &m.key,
                        &job,
                        &*policy,
                        instance,
                    ),
                    job,
                    role: m.role.clone(),
                    index: index_of[s.member],
                    primary: m.primary,
                    zone: caps.as_ref().and_then(|c| c.zone.clone()),
                    endpoint: caps.as_ref().and_then(|c| c.endpoint.clone()),
                })
            })
            .collect();

        let active = seats
            .iter()
            .map(|s| (s.worker.clone(), s.key.clone(), s.job.clone()))
            .collect();
        self.tracker.activate_members(attempt, active);
        for s in &seats {
            self.pool.assign_job(&s.worker, &s.key);
        }

        Some(crate::cluster::Committing { cluster, seats })
    }

    fn restore_placement(
        &mut self,
        cluster: crate::cluster::PendingCluster,
        seats: &[(String, String)],
    ) {
        for (worker, key) in seats {
            self.tracker.remove_active(key);
            self.pool.release_job(worker, key);
        }
        self.tracker.restore_cluster(cluster);
    }

    fn cluster_snapshot(&self, now: Instant) -> crate::cluster::ClusterSnapshot {
        let mut clusters: Vec<_> = self
            .tracker
            .ready_clusters()
            .filter(|c| c.not_before.is_none_or(|t| t <= now))
            .cloned()
            .collect();
        let mut slots: Vec<_> = self
            .idle
            .live(now)
            .filter_map(|(worker, kind)| {
                let (authorized, caps) = self.auth_and_caps(worker);
                let caps = caps?;
                Some(crate::cluster::Slot {
                    worker: worker.to_owned(),
                    kind,
                    zone: caps.zone.clone(),
                    caps,
                    authorized,
                })
            })
            .collect();
        clusters.sort_by_key(|c| (!c.prioritized(), c.queued_at, c.id));
        slots.sort_by(|a: &crate::cluster::Slot, b| (&a.worker, a.kind).cmp(&(&b.worker, b.kind)));
        let keys: HashSet<&str> = clusters
            .iter()
            .flat_map(|c| c.members.iter().map(|m| m.key.as_str()))
            .collect();
        let scores = self.tracker.member_scores(&keys);
        crate::cluster::ClusterSnapshot {
            clusters,
            slots,
            scores,
        }
    }
}

pub struct CoreActor;

impl Actor for CoreActor {
    type Msg = SchedulerMsg;
    type State = SchedulerCore;
    type Arguments = CoreArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(SchedulerCore {
            pool: WorkerPool::new(),
            tracker: JobTracker::new(),
            offers: 0,
            policy: args.policy,
            idle: Default::default(),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        core: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SchedulerMsg::Register(reg, reply) => {
                let last_seen = core.pool.register(
                    reg.worker.clone(),
                    reg.capabilities,
                    reg.authorized_peers,
                    reg.session,
                );
                for reattached in reg.active {
                    core.pool.assign_job(&reg.worker, &reattached.job_id);
                    core.tracker.restore_active(&reg.worker, reattached);
                }
                info!(worker = %reg.worker, "worker registered");
                let _ = reply.send(Registered { last_seen });
            }
            SchedulerMsg::Unregister { worker, reply } => {
                let orphaned = core.pool.unregister(&worker);
                core.idle.forget_worker(&worker);
                let gone = core.tracker.worker_disconnected(&worker);
                let total = orphaned.len() + gone.requeued.len() + gone.cluster_members.len();
                if total > 0 {
                    info!(%worker, orphaned_jobs = total, "worker disconnected; jobs re-queued");
                }
                let _ = reply.send(gone);
            }
            SchedulerMsg::IsConnected { worker, reply } => {
                let _ = reply.send(core.pool.is_connected(&worker));
            }
            SchedulerMsg::AuthorizedFor {
                worker,
                project,
                reply,
            } => {
                let ok = core
                    .pool
                    .peer_auth_for(&worker)
                    .map(|a| a.contains(&project))
                    .unwrap_or(false);
                let _ = reply.send(ok);
            }
            SchedulerMsg::UpdatePeers {
                worker,
                peers,
                reply,
            } => {
                core.pool.update_authorized_peers(&worker, peers);
                let _ = reply.send(());
            }
            SchedulerMsg::RevokePeers {
                worker,
                revoked,
                reply,
            } => {
                let job_ids = core.tracker.drain_peer_jobs_on_worker(&worker, &revoked);
                for job_id in &job_ids {
                    core.pool.send_abort(
                        &worker,
                        job_id.clone(),
                        "project deactivated worker".to_owned(),
                    );
                }
                if !job_ids.is_empty() {
                    core.bump_offers();
                }
                let _ = reply.send(job_ids.len());
            }
            SchedulerMsg::Reauth { worker } => core.pool.request_reauth(&worker),
            SchedulerMsg::UpdateCapabilities {
                worker,
                caps,
                reply,
            } => {
                core.pool.update_capabilities(&worker, caps);
                let _ = reply.send(());
            }
            SchedulerMsg::UpdateMetrics {
                worker,
                metrics,
                reply,
            } => {
                core.pool.update_metrics(
                    &worker,
                    metrics.cpu_usage_pct,
                    metrics.ram_free_mb,
                    metrics.disk_speed_mbps,
                    metrics.network_speed_mbps,
                );
                let _ = reply.send(());
            }
            SchedulerMsg::MarkDraining { worker, reply } => {
                core.pool.mark_draining(&worker);
                core.idle.forget_worker(&worker);
                let _ = reply.send(());
            }
            SchedulerMsg::Enqueue { job_id, job, reply } => {
                core.tracker.add_pending(job_id.clone(), job);
                core.pool.remove_sent_candidate(&job_id);
                core.bump_offers();
                let _ = reply.send(());
            }
            SchedulerMsg::EnqueueMember {
                of,
                key,
                job,
                reply,
            } => {
                core.tracker.add_member(of, key.clone(), job);
                core.pool.remove_sent_candidate(&key);
                core.bump_offers();
                let _ = reply.send(());
            }
            SchedulerMsg::ClusterSnapshot { reply } => {
                let _ = reply.send(core.cluster_snapshot(Instant::now()));
            }
            SchedulerMsg::TakePlacement {
                placement,
                attempt,
                instance,
                reply,
            } => {
                let _ = reply.send(core.take_placement(&placement, attempt, &instance));
            }
            SchedulerMsg::RestoreCluster {
                cluster,
                seats,
                reply,
            } => {
                core.restore_placement(cluster, &seats);
                core.bump_offers();
                let _ = reply.send(());
            }
            SchedulerMsg::SignalWorkers { signals } => {
                for (worker, signal) in signals {
                    core.pool.signal(&worker, signal);
                }
            }
            SchedulerMsg::Candidates {
                worker,
                only_new,
                reply,
            } => {
                let _ = reply.send(core.candidates(&worker, only_new));
            }
            SchedulerMsg::Assign {
                worker,
                kind,
                instance,
                reply,
            } => {
                let _ = reply.send(core.assign(&worker, &kind, &instance));
            }
            SchedulerMsg::RecordScores {
                worker,
                scores,
                reply,
            } => {
                core.tracker.record_scores(&worker, scores);
                let _ = reply.send(());
            }
            SchedulerMsg::Rejected {
                worker,
                job_id,
                reply,
            } => {
                core.pool.release_job(&worker, &job_id);
                core.tracker.release_to_pending(&job_id);
                core.pool.remove_sent_candidate(&job_id);
                info!(%worker, %job_id, "job rejected; re-queued");
                let _ = reply.send(());
            }
            SchedulerMsg::Release {
                worker,
                job_id,
                reply,
            } => {
                let worker_idle = core.pool.release_job(&worker, &job_id);
                let job = core.tracker.remove_active(&job_id);
                let _ = reply.send(Released { job, worker_idle });
            }
            SchedulerMsg::AbortJob {
                worker,
                job_id,
                reason,
                reply,
            } => {
                let sent = core.pool.send_abort(&worker, job_id.clone(), reason);
                if sent {
                    core.tracker.mark_aborting(&job_id, Instant::now());
                }

                let _ = reply.send(sent);
            }
            SchedulerMsg::ReapOverdueAborts { grace, reply } => {
                let reaped = core.tracker.take_overdue_aborts(Instant::now(), grace);
                for (worker, job_id) in &reaped {
                    core.pool.release_job(worker, job_id);
                }

                let _ = reply.send(reaped.into_iter().map(|(_, job_id)| job_id).collect());
            }
            SchedulerMsg::AbortEvaluation {
                evaluation_id,
                aborted_anchors,
                reply,
            } => {
                // Stop this evaluation's own eval job unconditionally, but a
                // build only when the database aborted its anchor: an anchor
                // shared with another live evaluation keeps building for it.
                let aborted_anchors: HashSet<DerivationBuildId> =
                    aborted_anchors.into_iter().collect();
                let to_abort: Vec<(String, String)> = core
                    .tracker
                    .active_jobs()
                    .filter(|(_, _, job)| job.evaluation_id() == evaluation_id)
                    .filter(|(_, _, job)| {
                        job.derivation_build()
                            .is_none_or(|anchor| aborted_anchors.contains(&anchor))
                    })
                    .map(|(job_id, worker, _)| (worker.to_owned(), job_id.to_owned()))
                    .collect();
                let now = Instant::now();
                for (worker, job_id) in &to_abort {
                    core.pool
                        .send_abort(worker, job_id.clone(), "evaluation aborted".to_owned());
                    core.tracker.mark_aborting(job_id, now);
                }
                core.tracker.remove_pending_for_evaluation(evaluation_id);
                let _ = reply.send(to_abort);
            }
            SchedulerMsg::Prioritize {
                evaluation,
                anchors,
                reply,
            } => {
                core.tracker
                    .prioritize(evaluation, &anchors.into_iter().collect());
                let _ = reply.send(());
            }
            SchedulerMsg::RemoveJobs { job_ids, reply } => {
                for id in &job_ids {
                    core.tracker.remove_job(id);
                }
                let _ = reply.send(());
            }
            SchedulerMsg::ActiveJob { job_id, reply } => {
                let _ = reply.send(core.tracker.active_job(&job_id).cloned());
            }
            SchedulerMsg::PendingJob { job_id, reply } => {
                let _ = reply.send(core.tracker.pending_job(&job_id).cloned());
            }
            SchedulerMsg::Untracked { job_ids, reply } => {
                let unknown = job_ids
                    .into_iter()
                    .filter(|id| !core.tracker.contains_job(id))
                    .collect();
                let _ = reply.send(unknown);
            }
            SchedulerMsg::PrunePendingBuilds { stale, reply } => {
                let _ = reply.send(core.tracker.prune_pending_builds(stale));
            }
            SchedulerMsg::HasIdleEvalOnlyWorker { reply } => {
                let _ = reply.send(core.pool.has_idle_eval_only_worker());
            }
            SchedulerMsg::Workers { reply } => {
                let _ = reply.send(core.pool.all_workers());
            }
            SchedulerMsg::WorkerCaps { worker, reply } => {
                let _ = reply.send(core.pool.gradient_caps_for(&worker));
            }
            SchedulerMsg::StaleWorkers {
                now_ms,
                timeout_ms,
                reply,
            } => {
                let _ = reply.send(core.pool.stale_worker_ids(now_ms, timeout_ms));
            }
            SchedulerMsg::Counts { reply } => {
                let (workers, idle_workers) = core.pool.worker_counts();
                let (active_builds, pending_builds) = core.tracker.instance_counts();
                let _ = reply.send(Counts {
                    workers: workers as usize,
                    idle_workers: idle_workers as usize,
                    pending: core.tracker.pending_count(),
                    active: core.tracker.active_count(),
                    pending_builds,
                    active_builds,
                    cpu_core_score_mean: core.pool.mean_cpu_core_score(),
                });
            }
            SchedulerMsg::PendingSnapshot { reply } => {
                let _ = reply.send(core.tracker.pending_snapshot());
            }
            SchedulerMsg::BoardActiveJobs { reply } => {
                let _ = reply.send(core.tracker.board_active_jobs());
            }
            SchedulerMsg::RecentDecisions { reply } => {
                let _ = reply.send(core.tracker.recent_decisions());
            }
            SchedulerMsg::CandidateDetail { id, reply } => {
                let _ = reply.send(core.tracker.candidate_detail(id));
            }
            SchedulerMsg::BumpRescore { reply } => {
                core.tracker.bump_rescore_counts();
                let _ = reply.send(());
            }
            SchedulerMsg::ReOffer => {
                if core.tracker.has_pending() {
                    core.bump_offers();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_tests::{eval_job, eval_worker_caps, port};

    #[test]
    fn a_job_that_left_the_pending_set_leaves_every_sent_set() {
        let mut core = SchedulerCore {
            pool: WorkerPool::new(),
            tracker: JobTracker::new(),
            offers: 0,
            policy: gradient_pool::score::policy_by_name("simple"),
            idle: Default::default(),
        };
        core.pool
            .register("w1".into(), eval_worker_caps(), HashSet::new(), port().0);
        let job = eval_job(ProjectId::now_v7());
        let job_id = crate::jobs::eval_job_key(job.evaluation_id);
        core.tracker
            .add_pending(job_id.clone(), PendingJob::Eval(job));
        assert_eq!(core.candidates("w1", true).candidates.len(), 1);

        core.tracker.remove_job(&job_id);
        core.candidates("w1", true);

        assert!(core.pool.sent_candidates_for("w1").unwrap().is_empty());
    }
}
