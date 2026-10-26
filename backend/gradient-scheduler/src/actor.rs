/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::{Duration, Instant};

use gradient_pool::score::{InstanceContext, ScoringPolicy};
use gradient_types::ids::{
    ClusterAttemptId, DerivationBuildId, DerivationId, DispatchedJobId, EvaluationId, ProjectId,
};
use gradient_wire::types::{
    BuildStage, CandidateScore, GradientCapabilities, JobCandidate, JobKind,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use tracing::{debug, info};

use crate::jobs::{
    AssignDecision, Assignment, AwaitingReclaim, BoardActiveJob, CandidateDetail, HeldReclaim,
    JobTracker, PendingBuildJob, PendingJob, PendingJobInfo, Reattached,
};
use gradient_db::maintenance::recovery::KeptAssignment;
use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_pool::{WorkerCaps, WorkerInfo, WorkerPool};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Registration {
    pub worker: String,
    pub capabilities: GradientCapabilities,
    pub authorized_peers: HashSet<ProjectId>,
    pub session: Arc<dyn SessionPort>,
    pub active: Vec<crate::jobs::Reattached>,
    pub reclaims_jobs: bool,
}

pub struct Registered {
    pub last_seen: Arc<AtomicI64>,
    pub unreclaimed: Vec<(String, AwaitingReclaim)>,
}

pub type WorkerCapabilities = gradient_pool::WorkerProfile;

#[derive(Debug, Clone)]
pub struct WorkerMetrics {
    pub cpu_usage_pct: f32,
    pub ram_free_mb: u64,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub build_peak_ram_mb: Option<Vec<(String, u64)>>,
    pub exits_at: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub candidates: Vec<JobCandidate>,
    pub projects: HashMap<String, ProjectId>,
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
    pub upload_speed_mean_mbps: Option<f64>,
    pub download_speed_mean_mbps: Option<f64>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one message per mailbox send; boxing only relocates the allocation"
)]
pub enum SchedulerMsg {
    Register(Registration, RpcReplyPort<Registered>),
    Unregister {
        worker: String,
        grace: Duration,
        reply: RpcReplyPort<crate::jobs::Disconnected>,
    },
    TakeUnreclaimed {
        now: Instant,
        reply: RpcReplyPort<Vec<(String, AwaitingReclaim)>>,
    },
    ReturnUnreclaimed {
        unreclaimed: Vec<(String, AwaitingReclaim)>,
        reply: RpcReplyPort<Vec<(String, AwaitingReclaim)>>,
    },
    Reclaim {
        worker: String,
        listed: Vec<DispatchedJobId>,
        reply: RpcReplyPort<HeldReclaim>,
    },
    RestoreReclaimed {
        worker: String,
        reattached: Vec<Reattached>,
        hold_again: Vec<(String, AwaitingReclaim)>,
        reply: RpcReplyPort<Vec<Reattached>>,
    },
    TakeUnlisted {
        worker: String,
        listed: HashSet<DispatchedJobId>,
        reply: RpcReplyPort<Vec<(String, AwaitingReclaim)>>,
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
        reply: RpcReplyPort<Vec<PendingJob>>,
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
    EnterStage {
        worker: String,
        job_id: String,
        stage: BuildStage,
    },
    Enqueue {
        job_id: String,
        job: PendingJob,
        reply: RpcReplyPort<()>,
    },
    EnqueueMember {
        of: gradient_db::scheduling::cluster::MemberOf,
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
        reply: RpcReplyPort<()>,
    },
    ClaimInputFetch {
        worker: String,
        fetch_id: String,
        signal: SessionSignal,
        reply: RpcReplyPort<bool>,
    },
    ReleaseInputFetch {
        worker: String,
        fetch_id: String,
        reply: RpcReplyPort<()>,
    },
    DropCluster {
        seats: Vec<(String, String)>,
        reply: RpcReplyPort<()>,
    },
    Reserve {
        reservation: crate::cluster::Reservation,
        reply: RpcReplyPort<bool>,
    },
    ReleaseReservation {
        cluster: gradient_types::ids::ClusterJobId,
    },
    Reservation {
        reply: RpcReplyPort<Option<crate::cluster::Reservation>>,
    },
    DropWaiting {
        cluster: gradient_types::ids::ClusterJobId,
        reply: RpcReplyPort<Option<crate::cluster::PendingCluster>>,
    },
    IsAborting {
        job_id: String,
        reply: RpcReplyPort<bool>,
    },
    ReturnMember {
        worker: String,
        key: String,
        assignment_id: DispatchedJobId,
        job: PendingJob,
        attempt: ClusterAttemptId,
        reply: RpcReplyPort<()>,
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
        /// A shared build is global. The ones the abort spared are still wanted by a live
        /// evaluation, and their workers must keep building.
        aborted_shared_builds: Vec<DerivationBuildId>,
        reply: RpcReplyPort<Vec<(String, String)>>,
    },
    Prioritize {
        evaluation: Option<EvaluationId>,
        shared_builds: Vec<DerivationBuildId>,
        reply: RpcReplyPort<()>,
    },
    CancelJobs {
        job_ids: Vec<String>,
        reply: RpcReplyPort<Vec<(String, String)>>,
    },
    ActiveJob {
        job_id: String,
        reply: RpcReplyPort<Option<PendingJob>>,
    },
    ActiveBuildDispatch {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<Option<(DispatchedJobId, PendingBuildJob)>>,
    },
    ActiveRunnerAttempt {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<Option<gradient_types::ids::RunnerJobAttemptId>>,
    },
    OwnsDispatch {
        worker: String,
        job_id: String,
        assignment_id: DispatchedJobId,
        reply: RpcReplyPort<bool>,
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
    PendingSharedBuilds {
        reply: RpcReplyPort<Vec<DerivationBuildId>>,
    },
    LastingRamShortages {
        reply: RpcReplyPort<Vec<gradient_graph::RamShortage>>,
    },
    EstimatesToRefresh {
        changes: crate::build_history::Changes,
        reply: RpcReplyPort<Vec<(String, gradient_pool::estimate::GroupKey, DerivationId)>>,
    },
    LearnEstimates {
        estimates: Vec<(String, gradient_pool::score::BuildEstimate)>,
    },
    LiftImports {
        checked: HashSet<DerivationBuildId>,
        lifted: HashSet<DerivationBuildId>,
        reply: RpcReplyPort<()>,
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
    #[cfg(test)]
    TrackedKeys {
        reply: RpcReplyPort<crate::jobs::TrackedKeys>,
    },
    #[cfg(test)]
    ActiveWorker {
        job_id: String,
        reply: RpcReplyPort<Option<String>>,
    },
    #[cfg(test)]
    SentCandidates {
        worker: String,
        reply: RpcReplyPort<Option<HashSet<String>>>,
    },
    #[cfg(test)]
    WaitingCluster {
        cluster: gradient_types::ids::ClusterJobId,
        reply: RpcReplyPort<Option<crate::cluster::PendingCluster>>,
    },
    RecentDecisions {
        reply: RpcReplyPort<Vec<AssignDecision>>,
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
    pub kept: Vec<KeptAssignment>,
    pub reclaim_grace: Duration,
}

pub struct SchedulerCore {
    pool: WorkerPool,
    tracker: JobTracker,
    offers: u64,
    policy: Arc<dyn ScoringPolicy>,
    idle: crate::cluster::IdleSlots,
    reservation: Option<crate::cluster::Reservation>,
    unmeasured_build_ram_mb: u64,
    ram_shortages: crate::ram_shortage::RamShortages,
}

impl SchedulerCore {
    fn bump_offers(&mut self) {
        self.offers = self.offers.wrapping_add(1);
        self.pool.signal_active(SessionSignal::Offers(self.offers));
    }

    fn abort_running(&mut self, keep: impl Fn(&str, &PendingJob) -> bool) -> Vec<(String, String)> {
        let running: Vec<(String, String)> = self
            .tracker
            .active_jobs()
            .filter(|(job_id, _, job)| keep(job_id, job))
            .map(|(job_id, worker, _)| (worker.to_owned(), job_id.to_owned()))
            .collect();
        let now = Instant::now();
        for (worker, job_id) in &running {
            self.pool
                .send_abort(worker, job_id.clone(), "evaluation aborted".to_owned());
            self.tracker.mark_aborting(job_id, now);
        }
        running
    }

    fn occupy(
        &mut self,
        worker: &str,
        job_id: &str,
        kind: JobKind,
        ram_need: gradient_pool::score::RamNeed,
    ) {
        let reserved_ram_mb = ram_need.reserved_mb(self.unmeasured_build_ram_mb);
        self.pool.assign_job(worker, job_id, kind, reserved_ram_mb);
    }

    fn return_to_pending(&mut self, job_ids: &[String]) {
        for job_id in job_ids {
            self.tracker.release_to_pending(job_id);
            self.pool.remove_sent_candidate(job_id);
        }
    }

    /// A build lost with its worker keeps the peak RAM the worker last reported for it. The
    /// next attempt reserves at least that much.
    fn unregister(&mut self, worker: &str, grace: Duration) -> crate::jobs::Disconnected {
        let observed_peak_ram_mb = self.pool.observed_build_ram(worker);
        let reclaims_jobs = self.pool.reclaims_jobs(worker);
        let orphaned = self.pool.unregister(worker);
        self.idle.forget_worker(worker);
        if self
            .reservation
            .as_ref()
            .is_some_and(|r| r.seats_worker(worker))
        {
            self.reservation = None;
        }
        self.tracker.raise_predicted_ram(&observed_peak_ram_mb);
        let gone = if reclaims_jobs && !grace.is_zero() {
            self.tracker
                .hold_for_reclaim(worker, Instant::now() + grace, &observed_peak_ram_mb)
        } else {
            self.tracker.worker_disconnected(worker)
        };
        self.return_to_pending(&gone.requeued_keys);
        let total = orphaned.len() + gone.requeued.len() + gone.cluster_members.len();
        if total > 0 {
            info!(%worker, orphaned_jobs = total, "worker disconnected; jobs re-queued");
        }
        crate::jobs::Disconnected {
            observed_peak_ram_mb,
            ..gone
        }
    }

    fn restore(&mut self, worker: &str, reattached: Vec<Reattached>) {
        for reattached in reattached {
            let job_id = reattached.job_id.clone();
            let kind = reattached.job.job_kind();
            let ram_need = reattached.job.ram_need_with(None, false);
            let devices = match &reattached.job {
                PendingJob::Runner(runner) => self
                    .pool
                    .worker_caps(worker)
                    .and_then(|caps| runner.requirement_for(&caps))
                    .map(|requirement| requirement.devices),
                PendingJob::Eval(_) | PendingJob::Build(_) => None,
            };
            if self.tracker.restore_active(worker, reattached) {
                self.occupy(worker, &job_id, kind, ram_need);
                if let Some(devices) = devices {
                    self.pool.hold_devices(worker, &job_id, devices);
                }
            }
        }
    }

    fn return_unreclaimed(&mut self, unreclaimed: &[(String, AwaitingReclaim)]) {
        self.tracker.return_unreclaimed(unreclaimed);
        for (job_id, _) in unreclaimed {
            self.pool.remove_sent_candidate(job_id);
        }
    }

    fn with_live_transfers(&self, instance: &InstanceContext) -> InstanceContext {
        InstanceContext {
            downloads_in_flight: self.pool.fleet_jobs_in(BuildStage::Prefetch),
            uploads_in_flight: self.pool.fleet_jobs_in(BuildStage::Upload),
            ..*instance
        }
    }

    fn auth_and_caps(&self, worker: &str) -> (Option<HashSet<ProjectId>>, Option<WorkerCaps>) {
        let authorized = self
            .pool
            .peer_auth_for(worker)
            .and_then(|a| a.as_filter())
            .cloned();
        (authorized, self.pool.worker_caps(worker))
    }

    fn lasting_ram_shortages(&mut self, now: Instant) -> Vec<gradient_graph::RamShortage> {
        let workers: Vec<crate::jobs::BuildWorker> = self
            .pool
            .all_workers()
            .into_iter()
            .filter_map(|worker| {
                let (authorized, caps) = self.auth_and_caps(&worker.id);
                caps.filter(|caps| caps.takes_jobs_of(&JobKind::Build))
                    .map(|caps| crate::jobs::BuildWorker { authorized, caps })
            })
            .collect();
        let current = self.tracker.builds_no_worker_fits(&workers);

        self.ram_shortages
            .lasting(current, now, crate::ram_shortage::RAM_SHORTAGE_GRACE)
    }

    fn candidates(&mut self, worker: &str, only_new: bool) -> Offer {
        let (authorized, caps) = self.auth_and_caps(worker);
        let mut offered = self
            .tracker
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        let visible: HashSet<String> = offered.iter().map(|(c, _)| c.job_id.clone()).collect();
        if only_new && let Some(sent) = self.pool.sent_candidates_for(worker) {
            offered.retain(|(c, _)| !sent.contains(&c.job_id));
        }
        self.pool.set_sent_candidates(worker, visible);
        let projects = offered
            .iter()
            .map(|(c, project)| (c.job_id.clone(), *project))
            .collect();
        Offer {
            candidates: offered.into_iter().map(|(c, _)| c).collect(),
            projects,
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
        if self
            .reservation
            .as_ref()
            .is_some_and(|r| r.holds(worker, slot))
        {
            return self.idle(worker, slot, caps.as_ref());
        }
        let policy = Arc::clone(&self.policy);
        let instance = self.with_live_transfers(instance);
        self.unmeasured_build_ram_mb = instance.unmeasured_build_ram_mb();
        match self.tracker.take_best_of_kind(
            worker,
            authorized.as_ref(),
            caps.as_ref(),
            kind,
            &*policy,
            &instance,
        ) {
            Some(assignment) => {
                self.idle.clear(worker, slot);
                self.occupy(
                    worker,
                    assignment.job_id(),
                    assignment.pending.job_kind(),
                    assignment.ram_need,
                );
                if let gradient_wire::types::Job::Runner(runner) = &assignment.job {
                    self.pool.hold_devices(
                        worker,
                        assignment.job_id(),
                        runner.requirement.devices.clone(),
                    );
                }
                AssignOutcome::Assigned(assignment)
            }
            None => self.idle(worker, slot, caps.as_ref()),
        }
    }

    fn idle(
        &mut self,
        worker: &str,
        slot: crate::cluster::SlotKind,
        caps: Option<&WorkerCaps>,
    ) -> AssignOutcome {
        if caps.is_some_and(|c| c.takes_jobs_of(&slot.job_kind())) {
            self.idle.record(worker, slot, Instant::now());
        }
        AssignOutcome::Nothing
    }

    fn take_placement(
        &mut self,
        placement: &crate::cluster::Placement,
        attempt: ClusterAttemptId,
        instance: &InstanceContext,
    ) -> Option<crate::cluster::Committing> {
        use crate::cluster::{CommittedSeat, SlotKind};

        let now = Instant::now();
        let seats_free = {
            let cluster = self
                .tracker
                .waiting_cluster(placement.cluster)
                .filter(|c| c.ready())?;
            placement.seats.iter().all(|s| {
                let kind = SlotKind::from(&cluster.members[s.member]);
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
        let instance = self.with_live_transfers(instance);
        self.unmeasured_build_ram_mb = instance.unmeasured_build_ram_mb();
        let seats: Vec<CommittedSeat> = placement
            .seats
            .iter()
            .filter_map(|s| {
                let m = &cluster.members[s.member];
                let job = m.job.clone()?;
                let caps = self.pool.worker_caps(&s.worker);
                self.idle.clear(&s.worker, SlotKind::from(m));
                Some(CommittedSeat {
                    worker: s.worker.clone(),
                    key: m.key.clone(),
                    record: self.tracker.member_record(
                        &s.worker,
                        caps.as_ref(),
                        &m.key,
                        &job,
                        &*policy,
                        &instance,
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
            .map(|s| {
                (
                    s.worker.clone(),
                    s.key.clone(),
                    s.record.assignment_id,
                    s.job.clone(),
                )
            })
            .collect();
        self.tracker.activate_members(attempt, active);
        for s in &seats {
            let need = s.job.ram_need_with(s.record.choice, false);
            self.occupy(&s.worker, &s.key, s.job.job_kind(), need);
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
            connected: self.connected_slots(),
            reservation: self.reservation.clone(),
        }
    }

    fn connected_slots(&self) -> Vec<crate::cluster::Slot> {
        use crate::cluster::{Slot, SlotKind};

        let mut slots = Vec::new();
        for worker in self.pool.all_workers().into_iter().filter(|w| !w.draining) {
            let (authorized, Some(caps)) = self.auth_and_caps(&worker.id) else {
                continue;
            };
            for kind in [SlotKind::Eval, SlotKind::Build] {
                if caps.takes_jobs_of(&kind.job_kind()) {
                    slots.push(Slot {
                        worker: worker.id.clone(),
                        kind,
                        zone: caps.zone.clone(),
                        caps: caps.clone(),
                        authorized: authorized.clone(),
                    });
                }
            }
        }
        slots.sort_by(|a, b| (&a.worker, a.kind).cmp(&(&b.worker, b.kind)));

        slots
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
        let mut tracker = JobTracker::new();
        let deadline = Instant::now() + args.reclaim_grace;
        for kept in args.kept {
            tracker.await_reclaim(
                AwaitingReclaim {
                    worker: kept.worker_id,
                    assignment_id: kept.assignment_id,
                    job: None,
                    deadline,
                    observed_peak_ram_mb: None,
                },
                kept.job_key,
            );
        }
        Ok(SchedulerCore {
            pool: WorkerPool::new(),
            tracker,
            offers: 0,
            policy: args.policy,
            idle: Default::default(),
            reservation: None,
            unmeasured_build_ram_mb: 0,
            ram_shortages: Default::default(),
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
                core.restore(&reg.worker, reg.active);
                core.pool.set_reclaims_jobs(&reg.worker, reg.reclaims_jobs);
                let unreclaimed = if reg.reclaims_jobs {
                    Vec::new()
                } else {
                    core.tracker.take_unlisted(&reg.worker, &HashSet::new())
                };
                core.return_unreclaimed(&unreclaimed);
                info!(worker = %reg.worker, "worker registered");
                let _ = reply.send(Registered {
                    last_seen,
                    unreclaimed,
                });
            }
            SchedulerMsg::Unregister {
                worker,
                grace,
                reply,
            } => {
                let _ = reply.send(core.unregister(&worker, grace));
            }
            SchedulerMsg::Reclaim {
                worker,
                listed,
                reply,
            } => {
                let taken = core.tracker.take_reclaimable(&worker, &listed);
                core.restore(&worker, taken.reattached.clone());
                let _ = reply.send(taken);
            }
            SchedulerMsg::RestoreReclaimed {
                worker,
                reattached,
                hold_again,
                reply,
            } => {
                core.tracker.hold_again(hold_again);
                let (refused, restored): (Vec<_>, Vec<_>) = reattached
                    .into_iter()
                    .partition(|job| core.tracker.contains_job(&job.job_id));
                core.restore(&worker, restored);
                let _ = reply.send(refused);
            }
            SchedulerMsg::TakeUnlisted {
                worker,
                listed,
                reply,
            } => {
                let unlisted = core.tracker.take_unlisted(&worker, &listed);
                core.return_unreclaimed(&unlisted);
                let _ = reply.send(unlisted);
            }
            SchedulerMsg::TakeUnreclaimed { now, reply } => {
                let _ = reply.send(core.tracker.take_unreclaimed(now));
            }
            SchedulerMsg::ReturnUnreclaimed { unreclaimed, reply } => {
                core.return_unreclaimed(&unreclaimed);
                let _ = reply.send(unreclaimed);
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
                let job_ids = core.tracker.peer_jobs_on_worker(&worker, &revoked);
                let jobs: Vec<PendingJob> = job_ids
                    .iter()
                    .filter_map(|id| core.tracker.active_job(id).cloned())
                    .collect();
                for job_id in &job_ids {
                    core.pool.send_abort(
                        &worker,
                        job_id.clone(),
                        "project deactivated worker".to_owned(),
                    );
                    core.pool.release_job(&worker, job_id);
                }
                core.return_to_pending(&job_ids);
                if !job_ids.is_empty() {
                    core.bump_offers();
                }
                let _ = reply.send(jobs);
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
                    metrics.upload_speed_mbps,
                    metrics.download_speed_mbps,
                );
                core.pool
                    .observe_build_ram(&worker, metrics.build_peak_ram_mb);
                let _ = reply.send(());
            }
            SchedulerMsg::MarkDraining { worker, reply } => {
                core.pool.mark_draining(&worker);
                core.idle.forget_worker(&worker);
                core.tracker.forget_waiting_build(&worker);
                let _ = reply.send(());
            }
            SchedulerMsg::EnterStage {
                worker,
                job_id,
                stage,
            } => core.pool.enter_stage(&worker, &job_id, stage),
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
                let taken = core.take_placement(&placement, attempt, &instance);
                if taken.is_some()
                    && core
                        .reservation
                        .as_ref()
                        .is_some_and(|r| r.cluster() == placement.cluster)
                {
                    core.reservation = None;
                }
                let _ = reply.send(taken);
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
            SchedulerMsg::DropWaiting { cluster, reply } => {
                let _ = reply.send(core.tracker.drop_cluster(cluster));
            }
            SchedulerMsg::IsAborting { job_id, reply } => {
                let _ = reply.send(core.tracker.is_aborting(&job_id));
            }
            SchedulerMsg::ReturnMember {
                worker,
                key,
                assignment_id,
                job,
                attempt,
                reply,
            } => {
                core.tracker
                    .activate_members(attempt, vec![(worker, key, assignment_id, job)]);
                let _ = reply.send(());
            }
            SchedulerMsg::Reserve { reservation, reply } => {
                let free = core
                    .reservation
                    .as_ref()
                    .is_none_or(|held| held.cluster() == reservation.cluster());
                if free {
                    info!(cluster = %reservation.cluster(), "cluster reserved its seats");
                    core.reservation = Some(reservation);
                }
                let _ = reply.send(free);
            }
            SchedulerMsg::ReleaseReservation { cluster } => {
                if core
                    .reservation
                    .as_ref()
                    .is_some_and(|r| r.cluster() == cluster)
                {
                    core.reservation = None;
                    core.bump_offers();
                }
            }
            SchedulerMsg::Reservation { reply } => {
                let _ = reply.send(core.reservation.clone());
            }
            SchedulerMsg::DropCluster { seats, reply } => {
                for (worker, key) in &seats {
                    core.tracker.remove_active(key);
                    core.pool.release_job(worker, key);
                }
                let _ = reply.send(());
            }
            SchedulerMsg::SignalWorkers { signals, reply } => {
                for (worker, signal) in signals {
                    core.pool.signal(&worker, signal);
                }
                let _ = reply.send(());
            }
            SchedulerMsg::ClaimInputFetch {
                worker,
                fetch_id,
                signal,
                reply,
            } => {
                let claimed = core.pool.has_capacity(&worker, &JobKind::Build);
                if claimed {
                    let key = crate::input_waits::input_fetch_key(&fetch_id);
                    core.pool.assign_job(&worker, &key, JobKind::Build, 0);
                    core.pool.signal(&worker, signal);
                }
                let _ = reply.send(claimed);
            }
            SchedulerMsg::ReleaseInputFetch {
                worker,
                fetch_id,
                reply,
            } => {
                core.pool
                    .release_job(&worker, &crate::input_waits::input_fetch_key(&fetch_id));
                let _ = reply.send(());
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
                core.return_to_pending(std::slice::from_ref(&job_id));
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
                aborted_shared_builds,
                reply,
            } => {
                let aborted = crate::jobs::AbortedJobs {
                    evaluation: evaluation_id,
                    shared_builds: aborted_shared_builds.into_iter().collect(),
                };
                let to_abort = core.abort_running(|_, job| aborted.contains(job));
                core.tracker.remove_waiting_jobs(&aborted);
                let _ = reply.send(to_abort);
            }
            SchedulerMsg::Prioritize {
                evaluation,
                shared_builds,
                reply,
            } => {
                core.tracker
                    .prioritize(evaluation, &shared_builds.into_iter().collect());
                let _ = reply.send(());
            }
            SchedulerMsg::CancelJobs { job_ids, reply } => {
                let running = core.abort_running(|job_id, _| job_ids.iter().any(|id| id == job_id));
                for id in job_ids
                    .iter()
                    .filter(|id| !running.iter().any(|(_, job_id)| job_id == *id))
                {
                    core.tracker.remove_job(id);
                }
                let _ = reply.send(running);
            }
            SchedulerMsg::ActiveJob { job_id, reply } => {
                let _ = reply.send(core.tracker.active_job(&job_id).cloned());
            }
            SchedulerMsg::ActiveRunnerAttempt {
                worker,
                job_id,
                reply,
            } => {
                let _ = reply.send(core.tracker.active_runner_attempt(&worker, &job_id));
            }
            SchedulerMsg::ActiveBuildDispatch {
                worker,
                job_id,
                reply,
            } => {
                let dispatch = core
                    .tracker
                    .active_build_dispatch(&worker, &job_id)
                    .map(|(assignment_id, build)| (assignment_id, build.clone()));
                let _ = reply.send(dispatch);
            }
            SchedulerMsg::OwnsDispatch {
                worker,
                job_id,
                assignment_id,
                reply,
            } => {
                let _ = reply.send(core.tracker.owns_dispatch(&worker, &job_id, assignment_id));
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
            SchedulerMsg::PendingSharedBuilds { reply } => {
                let _ = reply.send(core.tracker.pending_shared_builds());
            }
            SchedulerMsg::LastingRamShortages { reply } => {
                let _ = reply.send(core.lasting_ram_shortages(Instant::now()));
            }
            SchedulerMsg::EstimatesToRefresh { changes, reply } => {
                let _ = reply.send(core.tracker.estimates_to_refresh(&changes));
            }
            SchedulerMsg::LearnEstimates { estimates } => {
                core.tracker.learn_estimates(estimates);
            }
            SchedulerMsg::LiftImports {
                checked,
                lifted,
                reply,
            } => {
                core.tracker.lift_imports(&checked, &lifted);
                let _ = reply.send(());
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
                    upload_speed_mean_mbps: core.pool.mean_upload_speed_mbps(),
                    download_speed_mean_mbps: core.pool.mean_download_speed_mbps(),
                });
            }
            SchedulerMsg::PendingSnapshot { reply } => {
                let _ = reply.send(core.tracker.pending_snapshot());
            }
            SchedulerMsg::BoardActiveJobs { reply } => {
                let _ = reply.send(core.tracker.board_active_jobs());
            }
            #[cfg(test)]
            SchedulerMsg::TrackedKeys { reply } => {
                let _ = reply.send(core.tracker.tracked_keys());
            }
            #[cfg(test)]
            SchedulerMsg::ActiveWorker { job_id, reply } => {
                let _ = reply.send(core.tracker.active_worker(&job_id));
            }
            #[cfg(test)]
            SchedulerMsg::SentCandidates { worker, reply } => {
                let _ = reply.send(core.pool.sent_candidates_for(&worker).cloned());
            }
            #[cfg(test)]
            SchedulerMsg::WaitingCluster { cluster, reply } => {
                let _ = reply.send(core.tracker.waiting_cluster(cluster).cloned());
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
    use crate::test_jobs::{build_job, eval_job, eval_worker_caps, port};

    fn pending_heavy_build(core: &mut SchedulerCore, predicted_peak_ram_mb: u64) -> String {
        pending_build(core, predicted_peak_ram_mb, |_| {})
    }

    fn pending_build(
        core: &mut SchedulerCore,
        predicted_peak_ram_mb: u64,
        adjust: impl FnOnce(&mut crate::jobs::PendingBuildJob),
    ) -> String {
        let shared_build = DerivationBuildId::now_v7();
        let mut job = build_job(EvaluationId::now_v7(), ProjectId::now_v7(), shared_build);
        job.estimate =
            gradient_pool::score::BuildEstimate::fixed(Some(predicted_peak_ram_mb), None, 1);
        job.rescore_count = gradient_pool::score::weights::RESCORE_MAX_ROUNDS;
        adjust(&mut job);
        let job_id = crate::jobs::build_job_key(shared_build);
        core.tracker
            .add_pending(job_id.clone(), PendingJob::Build(job));
        job_id
    }

    fn core_with_a_128_gb_worker() -> SchedulerCore {
        let mut core = SchedulerCore {
            pool: WorkerPool::new(),
            tracker: JobTracker::new(),
            offers: 0,
            policy: gradient_pool::score::policy_for(gradient_types::ScoringPolicy::ResourceAware),
            idle: Default::default(),
            reservation: None,
            unmeasured_build_ram_mb: 0,
            ram_shortages: Default::default(),
        };
        let caps = GradientCapabilities {
            build: true,
            ..GradientCapabilities::default()
        };
        core.pool
            .register("w1".into(), caps, HashSet::new(), port().0);
        core.pool.update_capabilities(
            "w1",
            gradient_pool::WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 32,
                ram_total_mb: 128_000,
                ..Default::default()
            },
        );
        core.pool
            .update_metrics("w1", 0.0, 118_000, None, None, None);
        core
    }

    fn assigned_build(core: &mut SchedulerCore, worker: &str) -> Option<String> {
        match core.assign(worker, &JobKind::Build, &InstanceContext::default()) {
            AssignOutcome::Assigned(a) => Some(a.job_id().to_owned()),
            _ => None,
        }
    }

    #[test]
    fn a_burst_of_heavy_builds_stops_at_the_ram_of_the_worker() {
        let mut core = core_with_a_128_gb_worker();
        for _ in 0..3 {
            pending_heavy_build(&mut core, 50_000);
        }

        let first = assigned_build(&mut core, "w1").expect("an idle worker takes a build");
        assert!(assigned_build(&mut core, "w1").is_some());
        assert!(
            assigned_build(&mut core, "w1").is_none(),
            "a third build would overcommit the worker before its free RAM shows any load"
        );

        core.pool.release_job("w1", &first);
        assert!(assigned_build(&mut core, "w1").is_some());
    }

    #[test]
    fn a_held_heavy_build_keeps_its_ram_free_of_later_small_builds() {
        let mut core = core_with_a_128_gb_worker();
        pending_heavy_build(&mut core, 50_000);
        pending_heavy_build(&mut core, 50_000);
        let running = assigned_build(&mut core, "w1").expect("an idle worker takes a build");
        assert!(assigned_build(&mut core, "w1").is_some());

        let heavy = pending_build(&mut core, 60_000, |job| job.prioritized = true);
        let small = pending_heavy_build(&mut core, 10_000);
        assert_eq!(assigned_build(&mut core, "w1"), Some(small));

        pending_heavy_build(&mut core, 10_000);
        assert!(
            assigned_build(&mut core, "w1").is_none(),
            "a small build fitting the unreserved RAM must leave the RAM of the held build free"
        );

        core.pool.release_job("w1", &running);
        assert_eq!(assigned_build(&mut core, "w1"), Some(heavy));
    }

    #[test]
    fn a_briefly_held_build_leaves_the_ram_to_small_builds() {
        let mut core = core_with_a_128_gb_worker();
        pending_heavy_build(&mut core, 50_000);
        pending_heavy_build(&mut core, 50_000);
        assert!(assigned_build(&mut core, "w1").is_some());
        assert!(assigned_build(&mut core, "w1").is_some());

        pending_heavy_build(&mut core, 60_000);
        for _ in 0..2 {
            let small = pending_heavy_build(&mut core, 10_000);
            assert_eq!(assigned_build(&mut core, "w1"), Some(small));
        }
    }

    #[test]
    fn a_build_held_past_the_wait_keeps_its_ram_free() {
        let mut core = core_with_a_128_gb_worker();
        pending_heavy_build(&mut core, 50_000);
        pending_heavy_build(&mut core, 50_000);
        assert!(assigned_build(&mut core, "w1").is_some());
        assert!(assigned_build(&mut core, "w1").is_some());

        let wait = gradient_pool::score::weights::HELD_BUILD_RESERVES_RAM_AFTER_SECS;
        pending_build(&mut core, 60_000, |job| {
            job.ready_at -= chrono::Duration::seconds(wait);
        });
        let small = pending_heavy_build(&mut core, 10_000);
        assert_eq!(assigned_build(&mut core, "w1"), Some(small));

        pending_heavy_build(&mut core, 10_000);
        assert!(assigned_build(&mut core, "w1").is_none());
    }

    #[test]
    fn a_build_above_the_free_ram_of_an_idle_fleet_is_short_once_the_grace_passed() {
        let mut core = core_with_a_128_gb_worker();
        let large = pending_heavy_build(&mut core, 120_000);
        let start = Instant::now();
        let grace = crate::ram_shortage::RAM_SHORTAGE_GRACE;

        assert!(core.lasting_ram_shortages(start).is_empty());
        let lasting = core.lasting_ram_shortages(start + grace);

        assert_eq!(lasting.len(), 1, "{lasting:?}");
        assert_eq!(crate::jobs::build_job_key(lasting[0].shared_build), large);
        assert_eq!(lasting[0].largest_available_mb, 118_000);
    }

    #[test]
    fn a_large_build_is_not_short_while_the_worker_holds_ram_it_will_release() {
        let mut core = core_with_a_128_gb_worker();
        pending_heavy_build(&mut core, 120_000);
        let small = pending_heavy_build(&mut core, 10_000);
        assert_eq!(assigned_build(&mut core, "w1"), Some(small));
        let start = Instant::now();
        let grace = crate::ram_shortage::RAM_SHORTAGE_GRACE;

        core.lasting_ram_shortages(start);

        assert!(core.lasting_ram_shortages(start + grace).is_empty());
    }

    #[test]
    fn a_build_lost_with_its_worker_is_requeued_with_its_observed_peak() {
        let mut core = core_with_a_128_gb_worker();
        let build = pending_heavy_build(&mut core, 2_000);
        assert_eq!(assigned_build(&mut core, "w1"), Some(build.clone()));
        core.pool
            .observe_build_ram("w1", Some(vec![(build.clone(), 70_000)]));

        let gone = core.unregister("w1", Duration::ZERO);

        assert_eq!(gone.observed_peak_ram_mb.get(&build), Some(&70_000));
        let requeued = core.tracker.pending_job(&build).expect("requeued");
        assert!(
            matches!(
                requeued.ram_need_at(1, false),
                gradient_pool::score::RamNeed::Predicted(mb) if mb >= 70_000
            ),
            "the requeued build reserves at least its observed peak"
        );
    }

    #[test]
    fn a_job_that_left_the_pending_set_leaves_every_sent_set() {
        let mut core = SchedulerCore {
            pool: WorkerPool::new(),
            tracker: JobTracker::new(),
            offers: 0,
            policy: gradient_pool::score::policy_for(gradient_types::ScoringPolicy::Simple),
            idle: Default::default(),
            reservation: None,
            unmeasured_build_ram_mb: 0,
            ram_shortages: Default::default(),
        };
        core.pool
            .register("w1".into(), eval_worker_caps(), HashSet::new(), port().0);
        let job = eval_job(ProjectId::now_v7());
        let job_id = crate::jobs::eval_job_key(job.evaluation_id, 0);
        core.tracker
            .add_pending(job_id.clone(), PendingJob::Eval(job));
        assert_eq!(core.candidates("w1", true).candidates.len(), 1);

        core.tracker.remove_job(&job_id);
        core.candidates("w1", true);

        assert!(core.pool.sent_candidates_for("w1").unwrap().is_empty());
    }
}
